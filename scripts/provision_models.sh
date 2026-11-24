#!/usr/bin/env bash
set -euo pipefail

# Coral&Kai Companion Offline Model Provisioner
# Queries HuggingFace API dynamically via /usr/bin/curl (never Python SSL),
# downloads resumable local weights (.gguf, .bin, .onnx), validates sizes &
# magic headers offline via python3, and writes manifest.json + tokenizer JSONs.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

TARGET_MODELS_DIR="${HOME}/Library/Application Support/com.coralkai.companion/models"
MANIFEST_DIR=""
FORCE_DOWNLOAD=0

usage() {
  cat <<'EOF'
Usage: provision_models.sh [OPTIONS]

Optional user-side download helper for Coral&Kai Companion Bring-Your-Own-Model (BYOM).
Models are NOT bundled into src-tauri/resources/models/.

Options:
  --target-models-dir <dir>  Directory for binary model weights (.gguf, .bin, .onnx)
                             (default: ~/Library/Application Support/com.coralkai.companion/models)
  --manifest-dir <dir>       Directory for manifest.json & embed tokenizer/config JSONs
                             (default: same as --target-models-dir)
  --force                    Re-download binaries even if already present and valid
  -h, --help                 Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target-models-dir)
      TARGET_MODELS_DIR="$2"
      shift 2
      ;;
    --manifest-dir)
      MANIFEST_DIR="$2"
      shift 2
      ;;
    --force)
      FORCE_DOWNLOAD=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "${MANIFEST_DIR}" ]]; then
  MANIFEST_DIR="${TARGET_MODELS_DIR}"
fi

mkdir -p "${TARGET_MODELS_DIR}/llm" "${TARGET_MODELS_DIR}/asr" "${TARGET_MODELS_DIR}/embed"
mkdir -p "${MANIFEST_DIR}/llm" "${MANIFEST_DIR}/asr" "${MANIFEST_DIR}/embed"

TARGET_MODELS_DIR="$(cd "${TARGET_MODELS_DIR}" && pwd)"
MANIFEST_DIR="$(cd "${MANIFEST_DIR}" && pwd)"

CURL_BIN="/usr/bin/curl"
if [[ ! -x "${CURL_BIN}" ]]; then
  CURL_BIN="$(command -v curl)"
fi

PYTHON_BIN="$(command -v python3)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

log() {
  printf '[provision] %s\n' "$*"
}

fail() {
  printf '[provision][ERROR] %s\n' "$*" >&2
  exit 1
}

fetch_hf_metadata() {
  local repo="$1"
  local out_json="$2"
  local http_code
  http_code="$("${CURL_BIN}" -sS -L --retry 5 --retry-delay 2 \
    -o "${out_json}" -w "%{http_code}" \
    "https://huggingface.co/api/models/${repo}?blobs=true" || echo "000")"
  if [[ "${http_code}" == "200" ]]; then
    return 0
  fi
  return 1
}

# Download or resume file via curl -L --retry 5 -C -
# Idempotent when local size == expected_size (if expected_size > 0).
download_resumable() {
  local url="$1"
  local dest="$2"
  local expected_size="${3:-0}"

  mkdir -p "$(dirname "${dest}")"

  if [[ "${FORCE_DOWNLOAD}" -eq 1 ]]; then
    rm -f "${dest}"
  fi

  if [[ -f "${dest}" && "${expected_size}" -gt 0 ]]; then
    local current_size
    current_size="$("${PYTHON_BIN}" -c "import os,sys; print(os.path.getsize(sys.argv[1]))" "${dest}")"
    if [[ "${current_size}" -eq "${expected_size}" ]]; then
      log "Already downloaded (${current_size} bytes): ${dest}"
      return 0
    elif [[ "${current_size}" -gt "${expected_size}" ]]; then
      log "Existing file larger than expected (${current_size} > ${expected_size}), removing: ${dest}"
      rm -f "${dest}"
    else
      log "Resuming partial download (${current_size}/${expected_size} bytes): ${dest}"
    fi
  fi

  local curl_rc=0
  "${CURL_BIN}" -fL --retry 5 --retry-delay 3 -C - -o "${dest}" "${url}" || curl_rc=$?
  if [[ "${curl_rc}" -eq 33 && -f "${dest}" ]]; then
    # HTTP 416 Range Not Satisfiable can happen if file is already complete
    log "Server returned HTTP 416 on resume; verifying existing file ${dest}"
    return 0
  fi
  if [[ "${curl_rc}" -ne 0 ]]; then
    fail "curl failed (exit ${curl_rc}) downloading ${url}"
  fi
}

validate_llm_gguf() {
  local file="$1"
  "${PYTHON_BIN}" - "$file" <<'PY'
import os, sys
path = sys.argv[1]
if not os.path.isfile(path):
    sys.exit("Missing LLM file: " + path)
size = os.path.getsize(path)
min_bytes = 1024 * 1024 * 1024  # 1 GiB
if size <= min_bytes:
    sys.exit(f"LLM GGUF too small: {size} bytes (expected > {min_bytes})")
with open(path, "rb") as f:
    magic = f.read(4)
if magic != b"GGUF":
    sys.exit(f"Invalid GGUF magic bytes: {magic.hex()} (expected 47475546)")
print(f"VALID_GGUF size={size} magic={magic.hex()} ({magic.decode('ascii')})")
PY
}

validate_asr_ggml() {
  local file="$1"
  "${PYTHON_BIN}" - "$file" <<'PY'
import os, struct, sys
path = sys.argv[1]
if not os.path.isfile(path):
    sys.exit("Missing ASR file: " + path)
size = os.path.getsize(path)
min_bytes = 100 * 1024 * 1024  # 100 MiB
if size <= min_bytes:
    sys.exit(f"ASR model too small: {size} bytes (expected > {min_bytes})")
with open(path, "rb") as f:
    raw = f.read(4)
if len(raw) < 4:
    sys.exit("ASR file truncated")
magic_le = struct.unpack("<I", raw)[0]
if magic_le != 0x67676D6C:
    sys.exit(f"Invalid ggml magic: 0x{magic_le:08x} (raw={raw.hex()}, expected 0x67676d6c)")
print(f"VALID_GGML size={size} magic_le=0x{magic_le:08x} raw={raw.hex()}")
PY
}

validate_embed_onnx() {
  local file="$1"
  "${PYTHON_BIN}" - "$file" <<'PY'
import os, sys

path = sys.argv[1]
if not os.path.isfile(path):
    sys.exit("Missing ONNX file: " + path)
size = os.path.getsize(path)
min_bytes = 50 * 1024 * 1024  # 50 MiB
if size <= min_bytes:
    sys.exit(f"ONNX file too small: {size} bytes (expected > {min_bytes})")

# Verify protobuf wire format for onnx.ModelProto without loading whole file into RAM
with open(path, "rb") as f:
    head = f.read(8)
    f.seek(0)

    def read_varint():
        val = 0
        shift = 0
        while True:
            b_raw = f.read(1)
            if not b_raw:
                raise EOFError("Unexpected EOF in varint")
            b = b_raw[0]
            val |= (b & 0x7F) << shift
            if not (b & 0x80):
                return val
            shift += 7
            if shift > 63:
                raise ValueError("Varint overflow")

    fields_seen = set()
    ir_version = None
    has_graph = False
    valid_modelproto_fields = {1, 2, 3, 4, 5, 6, 7, 8, 14, 20}

    while f.tell() < size:
        tag = read_varint()
        field_num = tag >> 3
        wire_type = tag & 0x07
        if field_num == 0 or field_num not in valid_modelproto_fields:
            raise ValueError(f"Unexpected ModelProto field {field_num} at offset {f.tell()}")
        fields_seen.add(field_num)
        if wire_type == 0:  # varint
            val = read_varint()
            if field_num == 1:
                ir_version = val
        elif wire_type == 2:  # length-delimited
            length = read_varint()
            cur = f.tell()
            if cur + length > size:
                raise ValueError(f"Length-delimited field {field_num} exceeds EOF")
            if field_num == 7 and length > 1000:
                has_graph = True
            f.seek(length, os.SEEK_CUR)
        elif wire_type == 5:  # 32-bit
            if f.tell() + 4 > size:
                raise ValueError("32-bit field exceeds EOF")
            f.seek(4, os.SEEK_CUR)
        elif wire_type == 1:  # 64-bit
            if f.tell() + 8 > size:
                raise ValueError("64-bit field exceeds EOF")
            f.seek(8, os.SEEK_CUR)
        else:
            raise ValueError(f"Invalid protobuf wire type {wire_type}")

if ir_version is None or not (1 <= ir_version <= 30):
    raise ValueError(f"Invalid ONNX ir_version: {ir_version}")
if not has_graph:
    raise ValueError("ONNX ModelProto missing graph field")

print(f"VALID_ONNX size={size} ir_version={ir_version} fields={sorted(fields_seen)} head={head.hex()}")
PY
}

validate_json_file() {
  local file="$1"
  "${PYTHON_BIN}" - "$file" <<'PY'
import json, os, sys
path = sys.argv[1]
if not os.path.isfile(path) or os.path.getsize(path) == 0:
    sys.exit("Missing or empty JSON: " + path)
with open(path, "r", encoding="utf-8") as f:
    json.load(f)
PY
}

# ==============================================================================
# 1. LLM Provisioning (Primary + Fallbacks)
# ==============================================================================
provision_llm() {
  log "=== Provisioning LLM ==="
  local candidates=(
    "typhoon-ai/typhoon2.5-qwen3-4b-gguf"
    "Qwen/Qwen2.5-3B-Instruct-GGUF"
    "tensorblock/llama3.2-typhoon2-3b-instruct-GGUF"
  )

  local chosen_repo=""
  local chosen_file=""
  local chosen_size=0
  local chosen_license=""
  local chosen_arch=""
  local meta_json="${TMP_DIR}/llm_meta.json"
  local last_idx=$(( ${#candidates[@]} - 1 ))
  local last_repo="${candidates[${last_idx}]}"

  for repo in "${candidates[@]}"; do
    log "Querying HuggingFace API for LLM repo: ${repo}"
    if fetch_hf_metadata "${repo}" "${meta_json}"; then
      local parsed=""
      local is_q4km="0"
      local sel_file="${TMP_DIR}/llm_sel.txt"
      : > "${sel_file}"
      "${PYTHON_BIN}" - "${meta_json}" > "${sel_file}" <<'PY' || true
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    data = json.load(f)
siblings = data.get("siblings", [])
ggufs = [s for s in siblings if s.get("rfilename", "").lower().endswith(".gguf")]
# Prefer q4_k_m explicitly
q4km = [s for s in ggufs if "q4_k_m" in s["rfilename"].lower()]
target = q4km[0] if q4km else None
is_q4km = "1" if target is not None else "0"
# Fallback inside last-resort repo if q4_k_m doesn't exist
if target is None and ggufs:
    for pat in ("q4_0", "q3_k_m", "q5_k_m"):
        m = [s for s in ggufs if pat in s["rfilename"].lower()]
        if m:
            target = m[0]
            break
    if target is None:
        target = ggufs[0]
if target is None:
    sys.exit(1)
card = data.get("cardData") or {}
gguf_meta = data.get("gguf") or {}
lic = (card.get("license") or "unknown").replace("|", "/")
arch = (gguf_meta.get("architecture") or "unknown").replace("|", "/")
print(f"{target['rfilename']}|{target.get('size') or 0}|{lic}|{arch}|{is_q4km}")
PY
      parsed="$(cat "${sel_file}")"
      if [[ -n "${parsed}" ]]; then
        IFS='|' read -r chosen_file chosen_size chosen_license chosen_arch is_q4km <<< "${parsed}"
        # Prefer repos that actually carry q4_k_m unless we're on the last fallback
        if [[ "${is_q4km}" == "1" || "${repo}" == "${last_repo}" ]]; then
          chosen_repo="${repo}"
          break
        fi
        log "Repo ${repo} had no q4_k_m GGUF (${chosen_file}); checking next fallback"
      fi
    else
      log "Repo ${repo} unavailable; trying fallback..."
    fi
  done

  if [[ -z "${chosen_repo}" || -z "${chosen_file}" ]]; then
    fail "Could not resolve any valid LLM GGUF from candidate HuggingFace repositories"
  fi

  local url="https://huggingface.co/${chosen_repo}/resolve/main/${chosen_file}"
  local dest="${TARGET_MODELS_DIR}/llm/${chosen_file}"
  log "Selected LLM: ${chosen_repo} -> ${chosen_file} (${chosen_size} bytes)"

  download_resumable "${url}" "${dest}" "${chosen_size}"
  local val_out
  val_out="$(validate_llm_gguf "${dest}")"
  log "LLM verified: ${val_out}"

  local manifest_path="${MANIFEST_DIR}/llm/manifest.json"
  "${PYTHON_BIN}" - "${dest}" "${manifest_path}" "${chosen_repo}" "${chosen_file}" "${url}" "${chosen_license}" "${chosen_arch}" <<'PY'
import datetime, hashlib, json, os, sys

dest, manifest_path, repo, filename, url, lic, arch = sys.argv[1:8]
size_bytes = os.path.getsize(dest)
with open(dest, "rb") as f:
    magic = f.read(4).decode("ascii")
h = hashlib.sha256()
with open(dest, "rb") as f:
    for chunk in iter(lambda: f.read(4 * 1024 * 1024), b""):
        h.update(chunk)

# Match brain-llm formula: (size_mb * 1.25) + conservative kv_cache (~200MB at n_ctx=2048)
size_mb = size_bytes / (1024 * 1024)
estimated_ram_mb = int(round(size_mb * 1.25)) + 200
safety_tier = "safe8Gb" if estimated_ram_mb <= 3200 else ("tight8Gb" if estimated_ram_mb <= 4500 else "recommended16GbPlus")

model_id = os.path.splitext(filename)[0]
manifest = {
    "id": model_id,
    "name": "Typhoon 2.5 Qwen3 4B (Q4_K_M)" if "typhoon2.5" in filename.lower() else model_id,
    "kind": "llm",
    "repo": repo,
    "filename": filename,
    "url": url,
    "sizeBytes": size_bytes,
    "estimatedRamMb": estimated_ram_mb,
    "safetyTier": safety_tier,
    "magic": magic,
    "sha256": h.hexdigest(),
    "license": lic or "apache-2.0",
    "architecture": arch or "qwen3",
    "quantization": "Q4_K_M" if "q4_k_m" in filename.lower() else "GGUF",
    "languages": ["th", "en"],
    "provisionedAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
}
with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PY
  log "Wrote LLM manifest: ${manifest_path}"
}

# ==============================================================================
# 2. ASR Provisioning (ggerganov/whisper.cpp -> ggml-base.bin)
# ==============================================================================
provision_asr() {
  log "=== Provisioning ASR ==="
  local repo="ggerganov/whisper.cpp"
  local meta_json="${TMP_DIR}/asr_meta.json"

  fetch_hf_metadata "${repo}" "${meta_json}" || fail "Failed to query HuggingFace API for ${repo}"

  local asr_sel="${TMP_DIR}/asr_sel.txt"
  "${PYTHON_BIN}" - "${meta_json}" > "${asr_sel}" <<'PY'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    data = json.load(f)
for s in data.get("siblings", []):
    if s.get("rfilename") == "ggml-base.bin":
        print(f"{s['rfilename']}|{s.get('size') or 0}")
        sys.exit(0)
sys.exit("ggml-base.bin not found in ggerganov/whisper.cpp siblings")
PY

  local filename expected_size
  IFS='|' read -r filename expected_size < "${asr_sel}"

  local url="https://huggingface.co/${repo}/resolve/main/${filename}"
  local dest="${TARGET_MODELS_DIR}/asr/${filename}"
  log "Selected ASR: ${repo} -> ${filename} (${expected_size} bytes)"

  download_resumable "${url}" "${dest}" "${expected_size}"
  local val_out
  val_out="$(validate_asr_ggml "${dest}")"
  log "ASR verified: ${val_out}"

  local manifest_path="${MANIFEST_DIR}/asr/manifest.json"
  "${PYTHON_BIN}" - "${dest}" "${manifest_path}" "${repo}" "${filename}" "${url}" <<'PY'
import datetime, hashlib, json, os, struct, sys

dest, manifest_path, repo, filename, url = sys.argv[1:6]
size_bytes = os.path.getsize(dest)
with open(dest, "rb") as f:
    magic_le = struct.unpack("<I", f.read(4))[0]
h = hashlib.sha256()
with open(dest, "rb") as f:
    for chunk in iter(lambda: f.read(4 * 1024 * 1024), b""):
        h.update(chunk)

manifest = {
    "id": "whisper-ggml-base",
    "name": "Whisper Base (Multilingual Thai/English)",
    "kind": "asr",
    "repo": repo,
    "filename": filename,
    "url": url,
    "sizeBytes": size_bytes,
    "estimatedRamMb": 384,
    "safetyTier": "safe8Gb",
    "magic": f"0x{magic_le:08x}",
    "sha256": h.hexdigest(),
    "license": "mit",
    "languages": ["th", "en", "auto"],
    "provisionedAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
}
with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PY
  log "Wrote ASR manifest: ${manifest_path}"
}

# ==============================================================================
# 3. Embedding Model Provisioning (Xenova/paraphrase-multilingual-MiniLM-L12-v2)
# ==============================================================================
provision_embed() {
  log "=== Provisioning Embedding Model ==="
  local repo="Xenova/paraphrase-multilingual-MiniLM-L12-v2"
  local meta_json="${TMP_DIR}/embed_meta.json"

  fetch_hf_metadata "${repo}" "${meta_json}" || fail "Failed to query HuggingFace API for ${repo}"

  local embed_sel="${TMP_DIR}/embed_sel.txt"
  "${PYTHON_BIN}" - "${meta_json}" > "${embed_sel}" <<'PY'
import json, sys
required = [
    "onnx/model_quantized.onnx",
    "config.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
]
with open(sys.argv[1], "r", encoding="utf-8") as f:
    data = json.load(f)
by_name = {s.get("rfilename"): (s.get("size") or 0) for s in data.get("siblings", [])}
for req in required:
    if req not in by_name:
        sys.exit(f"Missing required sibling {req} in {data.get('id')}")
    print(f"{req}|{by_name[req]}")
PY

  local onnx_size=0
  while IFS='|' read -r rfilename rsize; do
    local url="https://huggingface.co/${repo}/resolve/main/${rfilename}"
    if [[ "${rfilename}" == "onnx/model_quantized.onnx" ]]; then
      onnx_size="${rsize}"
      local onnx_dest="${TARGET_MODELS_DIR}/embed/model_quantized.onnx"
      log "Downloading ONNX weights: ${rfilename} -> ${onnx_dest} (${rsize} bytes)"
      download_resumable "${url}" "${onnx_dest}" "${rsize}"
    else
      local json_dest="${MANIFEST_DIR}/embed/$(basename "${rfilename}")"
      log "Downloading tracked config/tokenizer JSON: ${rfilename} -> ${json_dest}"
      download_resumable "${url}" "${json_dest}" "${rsize}"
      validate_json_file "${json_dest}"
    fi
  done < "${embed_sel}"

  local onnx_dest="${TARGET_MODELS_DIR}/embed/model_quantized.onnx"
  local val_out
  val_out="$(validate_embed_onnx "${onnx_dest}")"
  log "Embed ONNX verified: ${val_out}"

  local manifest_path="${MANIFEST_DIR}/embed/manifest.json"
  "${PYTHON_BIN}" - "${onnx_dest}" "${manifest_path}" "${repo}" <<'PY'
import datetime, hashlib, json, os, sys

onnx_dest, manifest_path, repo = sys.argv[1:4]
size_bytes = os.path.getsize(onnx_dest)
h = hashlib.sha256()
with open(onnx_dest, "rb") as f:
    for chunk in iter(lambda: f.read(4 * 1024 * 1024), b""):
        h.update(chunk)

manifest = {
    "id": "paraphrase-multilingual-minilm-l12-v2",
    "name": "Multilingual MiniLM-L12-v2 (Quantized ONNX)",
    "kind": "embed",
    "repo": repo,
    "filename": "model_quantized.onnx",
    "sourcePath": "onnx/model_quantized.onnx",
    "url": f"https://huggingface.co/{repo}/resolve/main/onnx/model_quantized.onnx",
    "sizeBytes": size_bytes,
    "estimatedRamMb": 256,
    "dimensions": 384,
    "pooling": "mean",
    "normalize": True,
    "safetyTier": "safe8Gb",
    "sha256": h.hexdigest(),
    "files": [
        "model_quantized.onnx",
        "config.json",
        "tokenizer.json",
        "tokenizer_config.json",
        "special_tokens_map.json",
    ],
    "languages": ["th", "en", "multilingual"],
    "provisionedAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
}
with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PY
  log "Wrote Embed manifest: ${manifest_path}"
}

provision_llm
provision_asr
provision_embed

log "All models provisioned and verified successfully!"
