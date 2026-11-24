import {
  exportSoulMemoryWithPicker,
  formatSoulError,
  importSoulMemoryWithPicker,
  invokeCommand,
} from "../api/tauriClient";
import type { DreamDiary, MemoryFact, RelationshipState } from "../types/ipc";
import { ICONS } from "./icons";

export interface MemoryDrawerCallbacks {
  onClose: () => void;
  onRelationshipUpdated: (rel: RelationshipState) => void;
  onToast: (message: string, kind?: "info" | "error") => void;
}

function formatDateTime(tsMs: number): string {
  if (!tsMs) return "—";
  try {
    return new Date(tsMs).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return String(tsMs);
  }
}

export class MemoryDrawer {
  readonly root: HTMLElement;
  private readonly bodyEl: HTMLElement;
  private readonly callbacks: MemoryDrawerCallbacks;

  private facts: MemoryFact[] = [];
  private diaries: DreamDiary[] = [];
  private relationship: RelationshipState | null = null;
  private includeSuperseded = false;
  private consolidating = false;
  private migrating = false;
  private errorBanner: string | null = null;

  constructor(callbacks: MemoryDrawerCallbacks) {
    this.callbacks = callbacks;

    this.root = document.createElement("aside");
    this.root.className = "drawer hidden";
    this.root.setAttribute("aria-label", "Soul Memory Diary and Migration");

    const header = document.createElement("div");
    header.className = "drawer-header";

    const titleWrap = document.createElement("div");
    titleWrap.className = "drawer-title-wrap";
    const icon = document.createElement("span");
    icon.className = "drawer-title-icon";
    icon.innerHTML = ICONS.book;
    const h2 = document.createElement("h2");
    h2.textContent = "Soul Memory & Dream Diary";
    titleWrap.append(icon, h2);

    const closeBtn = document.createElement("button");
    closeBtn.type = "button";
    closeBtn.className = "icon-btn";
    closeBtn.title = "Close Memory Drawer";
    closeBtn.innerHTML = ICONS.close;
    closeBtn.addEventListener("click", () => this.callbacks.onClose());

    header.append(titleWrap, closeBtn);

    this.bodyEl = document.createElement("div");
    this.bodyEl.className = "drawer-body";

    this.root.append(header, this.bodyEl);
  }

  open(): void {
    this.root.classList.remove("hidden");
    void this.refresh();
  }

  close(): void {
    this.root.classList.add("hidden");
  }

  isOpen(): boolean {
    return !this.root.classList.contains("hidden");
  }

  async refresh(): Promise<void> {
    try {
      const [facts, diaries, rel] = await Promise.all([
        invokeCommand("list_memory_facts", {
          limit: 120,
          includeSuperseded: this.includeSuperseded,
        }),
        invokeCommand("list_dream_diaries", { limit: 30 }),
        invokeCommand("get_relationship_state"),
      ]);
      this.facts = facts;
      this.diaries = diaries;
      this.relationship = rel;
      this.callbacks.onRelationshipUpdated(rel);
      this.errorBanner = null;
      this.render();
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.render();
    }
  }

  private async handleConsolidateDreams(): Promise<void> {
    this.consolidating = true;
    this.errorBanner = null;
    this.render();
    try {
      const diary = await invokeCommand("consolidate_dreams_now");
      await this.refresh();
      if (diary) {
        this.callbacks.onToast("Memories consolidated into a new Dream Diary entry.");
      } else {
        this.callbacks.onToast("No new unconsolidated turns to dream about yet.");
      }
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.consolidating = false;
      this.render();
    }
  }

  private async handleExport(): Promise<void> {
    this.migrating = true;
    this.render();
    try {
      const savedPath = await exportSoulMemoryWithPicker();
      if (savedPath) {
        this.callbacks.onToast(`Exported soul_memory snapshot to ${savedPath}`);
      }
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.migrating = false;
      this.render();
    }
  }

  private async handleImport(): Promise<void> {
    this.migrating = true;
    this.render();
    try {
      const count = await importSoulMemoryWithPicker();
      if (count !== null) {
        await this.refresh();
        this.callbacks.onToast(`Restored ${count} memories from snapshot.`);
      }
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.migrating = false;
      this.render();
    }
  }

  private renderBondCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card bond-card";

    const rel = this.relationship ?? {
      trust: 0.3,
      warmth: 0.4,
      familiarity: 0.1,
      moodBaseline: 0.5,
      totalTurns: 0,
      lastInteractionAt: 0,
    };

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = "Companion Bond & Mood";
    const turnsPill = document.createElement("span");
    turnsPill.className = "pill pill-soft";
    turnsPill.textContent = `${rel.totalTurns} turns`;
    head.append(h3, turnsPill);

    const grid = document.createElement("div");
    grid.className = "bond-grid";

    const metrics: Array<{ label: string; value: number }> = [
      { label: "Trust", value: rel.trust },
      { label: "Warmth", value: rel.warmth },
      { label: "Familiarity", value: rel.familiarity },
      { label: "Mood Baseline", value: rel.moodBaseline },
    ];

    for (const m of metrics) {
      const row = document.createElement("div");
      row.className = "bond-metric";
      const top = document.createElement("div");
      top.className = "bond-metric-top";
      const k = document.createElement("span");
      k.textContent = m.label;
      const v = document.createElement("strong");
      v.textContent = `${Math.round(m.value * 100)}%`;
      top.append(k, v);

      const track = document.createElement("div");
      track.className = "mini-bar-track";
      const fill = document.createElement("div");
      fill.className = "mini-bar-fill";
      fill.style.width = `${Math.round(Math.max(0, Math.min(1, m.value)) * 100)}%`;
      track.appendChild(fill);

      row.append(top, track);
      grid.appendChild(row);
    }

    card.append(head, grid);
    return card;
  }

  private renderMigrationCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card";

    const title = document.createElement("h3");
    title.className = "card-subtitle";
    title.textContent = "Portable Soul Migration (soul_memory.sqlite)";

    const desc = document.createElement("p");
    desc.className = "model-notes";
    desc.textContent =
      "Back up or transfer all memories, dream diaries, and bond warmth to a new Mac in one click.";

    const btnRow = document.createElement("div");
    btnRow.className = "migration-actions";

    const exportBtn = document.createElement("button");
    exportBtn.type = "button";
    exportBtn.className = "btn-secondary btn-sm";
    exportBtn.disabled = this.migrating;
    exportBtn.innerHTML = `${ICONS.exportDb} <span>Export soul_memory.sqlite</span>`;
    exportBtn.addEventListener("click", () => void this.handleExport());

    const importBtn = document.createElement("button");
    importBtn.type = "button";
    importBtn.className = "btn-secondary btn-sm";
    importBtn.disabled = this.migrating;
    importBtn.innerHTML = `${ICONS.importDb} <span>Import soul_memory.sqlite</span>`;
    importBtn.addEventListener("click", () => void this.handleImport());

    btnRow.append(exportBtn, importBtn);
    card.append(title, desc, btnRow);
    return card;
  }

  private renderDreamSection(): HTMLElement {
    const section = document.createElement("section");
    section.className = "drawer-section";

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = "Dream Diary";

    const dreamBtn = document.createElement("button");
    dreamBtn.type = "button";
    dreamBtn.className = "btn-primary btn-sm";
    dreamBtn.disabled = this.consolidating;
    dreamBtn.innerHTML = `${ICONS.moon} <span></span>`;
    const btnLabel = dreamBtn.querySelector("span");
    if (btnLabel) {
      btnLabel.textContent = this.consolidating
        ? "Dreaming..."
        : "Sleep & Consolidate Memories Now";
    }
    dreamBtn.addEventListener("click", () => void this.handleConsolidateDreams());

    head.append(h3, dreamBtn);
    section.appendChild(head);

    if (this.diaries.length === 0) {
      const empty = document.createElement("p");
      empty.className = "empty-hint";
      empty.textContent = "No dream reflections recorded yet. Chat with Mina and tap Sleep above!";
      section.appendChild(empty);
      return section;
    }

    for (const d of this.diaries) {
      const card = document.createElement("article");
      card.className = "diary-item";

      const meta = document.createElement("div");
      meta.className = "diary-meta";

      const period = document.createElement("span");
      period.className = "pill pill-soft";
      period.textContent = `${d.periodType} · ${d.periodKey}`;

      const tone = document.createElement("span");
      tone.className = "pill pill-tone";
      const deltaSign = d.bondDelta >= 0 ? "+" : "";
      tone.textContent = `${d.emotionalTone} (${deltaSign}${(d.bondDelta * 100).toFixed(1)}% bond)`;

      meta.append(period, tone);

      const summary = document.createElement("p");
      summary.className = "diary-summary";
      summary.textContent = d.summary;

      const stamp = document.createElement("div");
      stamp.className = "diary-date";
      stamp.textContent = formatDateTime(d.createdAt);

      card.append(meta, summary, stamp);
      section.appendChild(card);
    }

    return section;
  }

  private renderFactsSection(): HTMLElement {
    const section = document.createElement("section");
    section.className = "drawer-section";

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = `Remembered Facts (${this.facts.length})`;

    const toggleLabel = document.createElement("label");
    toggleLabel.className = "checkbox-pill";
    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.checked = this.includeSuperseded;
    checkbox.addEventListener("change", () => {
      this.includeSuperseded = checkbox.checked;
      void this.refresh();
    });
    const span = document.createElement("span");
    span.textContent = "Inspect Superseded";
    toggleLabel.append(checkbox, span);

    head.append(h3, toggleLabel);
    section.appendChild(head);

    if (this.facts.length === 0) {
      const empty = document.createElement("p");
      empty.className = "empty-hint";
      empty.textContent = "No remembered facts yet. Tell Mina your favorite drink, hobbies, or name!";
      section.appendChild(empty);
      return section;
    }

    for (const f of this.facts) {
      const isSuperseded = f.supersededBy !== null;
      const item = document.createElement("div");
      item.className = `fact-item ${isSuperseded ? "superseded" : ""}`;

      const top = document.createElement("div");
      top.className = "fact-top";

      const cat = document.createElement("span");
      cat.className = "fact-cat";
      cat.textContent = `${f.category} · ${f.key}`;

      const status = document.createElement("span");
      status.className = isSuperseded ? "pill pill-muted" : "pill pill-ok";
      status.textContent = isSuperseded
        ? `Superseded ${formatDateTime(f.supersededAt ?? f.lastAccessedAt)}`
        : `Heat ${Math.round(f.memoryHeat * 100)}%`;

      top.append(cat, status);

      const val = document.createElement("div");
      val.className = "fact-val";
      val.textContent = f.value;

      const sub = document.createElement("div");
      sub.className = "fact-sub";
      sub.textContent = `Confidence ${Math.round(f.confidence * 100)}% · Recalled ${f.accessCount}× · ${formatDateTime(
        f.lastAccessedAt
      )}`;

      item.append(top, val, sub);
      section.appendChild(item);
    }

    return section;
  }

  render(): void {
    this.bodyEl.innerHTML = "";

    if (this.errorBanner) {
      const err = document.createElement("div");
      err.className = "error-banner";
      err.textContent = this.errorBanner;
      this.bodyEl.appendChild(err);
    }

    this.bodyEl.appendChild(this.renderBondCard());
    this.bodyEl.appendChild(this.renderDreamSection());
    this.bodyEl.appendChild(this.renderFactsSection());
    this.bodyEl.appendChild(this.renderMigrationCard());
  }
}
