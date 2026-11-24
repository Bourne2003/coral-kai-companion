import fs from "node:fs";
import path from "node:path";
import type { IncomingMessage, ServerResponse } from "node:http";
import { defineConfig, type Plugin } from "vite";

const MIME_BY_EXT: Record<string, string> = {
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml; charset=utf-8",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".moc3": "application/octet-stream",
};

function copyDirRecursive(srcDir: string, destDir: string): void {
  if (!fs.existsSync(srcDir)) return;
  fs.mkdirSync(destDir, { recursive: true });
  for (const entry of fs.readdirSync(srcDir, { withFileTypes: true })) {
    if (entry.name.startsWith(".")) continue;
    const src = path.join(srcDir, entry.name);
    const dst = path.join(destDir, entry.name);
    if (entry.isDirectory()) {
      copyDirRecursive(src, dst);
    } else if (entry.isFile()) {
      fs.copyFileSync(src, dst);
    }
  }
}

function createLive2dMiddleware(live2dRoot: string) {
  return (req: IncomingMessage, res: ServerResponse, next: () => void) => {
    const rawUrl = (req.url || "").split("?")[0];
    if (!rawUrl.startsWith("/live2d/")) {
      next();
      return;
    }
    const rel = decodeURIComponent(rawUrl.slice("/live2d/".length));
    const resolved = path.resolve(live2dRoot, rel);
    if (!resolved.startsWith(live2dRoot + path.sep) || !fs.existsSync(resolved) || !fs.statSync(resolved).isFile()) {
      next();
      return;
    }
    const ext = path.extname(resolved).toLowerCase();
    res.setHeader("Content-Type", MIME_BY_EXT[ext] ?? "application/octet-stream");
    fs.createReadStream(resolved).pipe(res);
  };
}

function bundledLive2dPlugin(): Plugin {
  const live2dRoot = path.resolve(__dirname, "../src-tauri/resources/live2d");
  const distLive2dDir = path.resolve(__dirname, "dist/live2d");

  return {
    name: "soulmate2d-bundled-live2d",
    configureServer(server) {
      server.middlewares.use(createLive2dMiddleware(live2dRoot));
    },
    configurePreviewServer(server) {
      server.middlewares.use(createLive2dMiddleware(live2dRoot));
    },
    writeBundle() {
      copyDirRecursive(live2dRoot, distLive2dDir);
    },
  };
}

export default defineConfig({
  clearScreen: false,
  plugins: [bundledLive2dPlugin()],
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    outDir: "dist",
    target: ["es2022", "safari16"],
    minify: "esbuild",
  },
});
