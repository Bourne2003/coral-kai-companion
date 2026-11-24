import { invokeIpc, type SystemRamStatus } from "./types/ipc";

async function initShell(): Promise<void> {
  const root = document.getElementById("app");
  if (!root) return;

  root.innerHTML = `
    <main style="font-family: -apple-system, BlinkMacSystemFont, sans-serif; padding: 16px; color: #f5f5f7; background: rgba(18, 18, 28, 0.85); border-radius: 16px;">
      <h1 style="margin: 0 0 8px; font-size: 18px;">SoulMate2D</h1>
      <p id="ram-status" style="margin: 0; font-size: 13px; opacity: 0.85;">Initializing runtime...</p>
    </main>
  `;

  const statusEl = document.getElementById("ram-status");
  if (!statusEl) return;

  try {
    const ram: SystemRamStatus = await invokeIpc("get_ram_status");
    statusEl.textContent = `RAM: ${ram.availableRamMb} MB free / ${ram.totalRamMb} MB total`;
  } catch {
    statusEl.textContent = "SoulMate2D UI shell ready (browser preview mode)";
  }
}

void initShell();
