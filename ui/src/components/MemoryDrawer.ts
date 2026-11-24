import {
  exportSoulMemoryWithPicker,
  formatSoulError,
  importSoulMemoryWithPicker,
  invokeCommand,
} from "../api/tauriClient";
import { ALL_EMOTIONS, EMOTION_META } from "../avatar/types";
import type {
  CharacterId,
  DiaryEntry,
  DreamDiary,
  EmotionProfileState,
  EmotionTag,
  MemoryFact,
  PersonaBio,
  RelationshipState,
  UserProfile,
} from "../types/ipc";
import { ICONS } from "./icons";

export interface MemoryDrawerCallbacks {
  onClose: () => void;
  onRelationshipUpdated: (rel: RelationshipState) => void;
  onToast: (message: string, kind?: "info" | "error") => void;
  getCompanionName?: () => string;
  getCharacterId?: () => CharacterId;
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
  private diaryEntries: DiaryEntry[] = [];
  private emotionProfile: EmotionProfileState | null = null;
  private personaBio: PersonaBio | null = null;
  private userProfile: UserProfile | null = null;
  private relationship: RelationshipState | null = null;

  private includeSuperseded = false;
  private consolidating = false;
  private migrating = false;
  private diaryFormOpen = false;
  private savingDiary = false;
  private savingBio = false;
  private savingUserProfile = false;
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
    h2.textContent = "Soul Memory & Diary";
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
      const [
        facts,
        diaries,
        rel,
        emotionProfile,
        diaryEntries,
        personaBio,
        userProfile,
      ] = await Promise.all([
        invokeCommand("list_memory_facts", {
          limit: 120,
          includeSuperseded: this.includeSuperseded,
        }),
        invokeCommand("list_dream_diaries", { limit: 30 }),
        invokeCommand("get_relationship_state"),
        invokeCommand("get_emotion_profile"),
        invokeCommand("list_diary_entries", { limit: 30 }),
        invokeCommand("get_persona_bio"),
        invokeCommand("get_user_profile"),
      ]);
      this.facts = facts;
      this.diaries = diaries;
      this.relationship = rel;
      this.emotionProfile = emotionProfile;
      this.diaryEntries = diaryEntries;
      this.personaBio = personaBio;
      this.userProfile = userProfile;
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
        this.callbacks.onToast("Memories compacted into Diary & Emotion state.");
      } else {
        this.callbacks.onToast("Memory compaction checked (no pending turns).");
      }
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.consolidating = false;
      this.render();
    }
  }

  private async handleAddDiaryEntry(
    title: string,
    content: string,
    mood: EmotionTag
  ): Promise<void> {
    if (!title.trim() || !content.trim()) {
      this.callbacks.onToast("Please enter both a diary title and content.", "error");
      return;
    }
    this.savingDiary = true;
    this.errorBanner = null;
    this.render();
    try {
      await invokeCommand("add_diary_entry", {
        title: title.trim(),
        content: content.trim(),
        mood,
      });
      this.diaryFormOpen = false;
      await this.refresh();
      this.callbacks.onToast("Saved new personal diary entry.");
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.savingDiary = false;
      this.render();
    }
  }

  private async handleSavePersonaBio(
    traits: string,
    selfReflection: string,
    favoriteTopics: string
  ): Promise<void> {
    const activeChar: CharacterId =
      this.personaBio?.characterId ?? this.callbacks.getCharacterId?.() ?? "coral";
    const displayName =
      this.personaBio?.displayName ??
      (activeChar === "kai" ? "Kai · ไค" : "Coral · คอรัล");

    this.savingBio = true;
    this.errorBanner = null;
    this.render();
    try {
      const updated = await invokeCommand("update_persona_bio", {
        bio: {
          characterId: activeChar,
          displayName,
          traits: traits.trim(),
          selfReflection: selfReflection.trim(),
          favoriteTopics: favoriteTopics.trim(),
          updatedAt: Date.now(),
        },
      });
      this.personaBio = updated;
      this.callbacks.onToast(`Saved ${this.callbacks.getCompanionName?.() || activeChar} Self-Bio.`);
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.savingBio = false;
      this.render();
    }
  }

  private async handleSaveUserProfile(
    displayName: string,
    timezone: string,
    location: string,
    notes: string
  ): Promise<void> {
    this.savingUserProfile = true;
    this.errorBanner = null;
    this.render();
    try {
      const updated = await invokeCommand("update_user_profile", {
        profile: {
          displayName: displayName.trim(),
          timezone: timezone.trim(),
          location: location.trim(),
          notes: notes.trim(),
          updatedAt: Date.now(),
        },
      });
      this.userProfile = updated;
      this.callbacks.onToast("Saved User Profile.");
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.savingUserProfile = false;
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
    const companionName = this.callbacks.getCompanionName?.() || "Coral";
    h3.textContent = `${companionName} Bond & Mood`;
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

  private renderEmotionProfileCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card emotion-decay-card";

    const profile = this.emotionProfile;
    const charLabel =
      profile?.characterId === "kai"
        ? "Kai"
        : profile?.characterId === "coral"
        ? "Coral"
        : this.callbacks.getCompanionName?.() || "Coral";

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = `7-Emotion Decay State (${charLabel})`;

    const domMeta = profile ? EMOTION_META[profile.dominant] : EMOTION_META.neutral;
    const domPill = document.createElement("span");
    domPill.className = "pill";
    domPill.style.background = domMeta.pillBg;
    domPill.style.color = domMeta.accentColor;
    domPill.textContent = profile
      ? `Dominant: ${domMeta.labelEn} (${Math.round(profile.dominantIntensity * 100)}%)`
      : "Loading...";
    head.append(h3, domPill);
    card.appendChild(head);

    if (!profile) {
      const hint = document.createElement("p");
      hint.className = "empty-hint";
      hint.textContent = "Computing live emotion half-life decay...";
      card.appendChild(hint);
      return card;
    }

    const list = document.createElement("div");
    list.className = "emotion-decay-list";

    const byTag = new Map(profile.emotions.map((e) => [e.tag, e]));
    for (const tag of ALL_EMOTIONS) {
      const entry = byTag.get(tag) ?? {
        tag,
        intensity: 0.1,
        baseline: 0.1,
        halfLifeMinutes: 45,
        updatedAtMs: profile.computedAtMs,
      };
      const meta = EMOTION_META[tag];
      const pct = Math.round(Math.max(0, Math.min(1, entry.intensity)) * 100);
      const basePct = Math.round(Math.max(0, Math.min(1, entry.baseline)) * 100);

      const row = document.createElement("div");
      row.className = "emotion-decay-row";

      const labelWrap = document.createElement("div");
      labelWrap.className = "emotion-decay-label";
      const dot = document.createElement("span");
      dot.className = "emotion-dot";
      dot.style.background = meta.accentColor;
      const name = document.createElement("span");
      name.textContent = `${meta.labelEn}`;
      labelWrap.append(dot, name);

      const track = document.createElement("div");
      track.className = "mini-bar-track emotion-decay-track";
      const fill = document.createElement("div");
      fill.className = "mini-bar-fill";
      fill.style.width = `${pct}%`;
      fill.style.background = meta.accentColor;
      track.appendChild(fill);

      const valWrap = document.createElement("div");
      valWrap.className = "emotion-decay-meta";
      const pctEl = document.createElement("strong");
      pctEl.textContent = `${pct}%`;
      const hlEl = document.createElement("span");
      hlEl.className = "halflife-badge";
      hlEl.title = `Baseline ${basePct}% · Half-life ${entry.halfLifeMinutes} min`;
      hlEl.textContent = `t½ ${entry.halfLifeMinutes}m`;
      valWrap.append(pctEl, hlEl);

      row.append(labelWrap, track, valWrap);
      list.appendChild(row);
    }

    card.appendChild(list);
    return card;
  }

  private renderPersonalDiarySection(): HTMLElement {
    const section = document.createElement("section");
    section.className = "drawer-section";

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = `Personal Diary (${this.diaryEntries.length})`;

    const actions = document.createElement("div");
    actions.className = "diary-head-actions";

    const writeBtn = document.createElement("button");
    writeBtn.type = "button";
    writeBtn.className = "btn-secondary btn-xs";
    writeBtn.textContent = this.diaryFormOpen ? "Cancel" : "+ Write Diary Entry";
    writeBtn.addEventListener("click", () => {
      this.diaryFormOpen = !this.diaryFormOpen;
      this.render();
    });

    const sleepBtn = document.createElement("button");
    sleepBtn.type = "button";
    sleepBtn.className = "btn-primary btn-xs";
    sleepBtn.disabled = this.consolidating;
    sleepBtn.innerHTML = `${ICONS.moon} <span></span>`;
    const btnLabel = sleepBtn.querySelector("span");
    if (btnLabel) {
      btnLabel.textContent = this.consolidating
        ? "Compacting..."
        : "Sleep & Compact Memories Now";
    }
    sleepBtn.addEventListener("click", () => void this.handleConsolidateDreams());

    actions.append(writeBtn, sleepBtn);
    head.append(h3, actions);
    section.appendChild(head);

    if (this.diaryFormOpen) {
      const form = document.createElement("form");
      form.className = "drawer-card diary-inline-form";

      const titleInput = document.createElement("input");
      titleInput.type = "text";
      titleInput.className = "drawer-input";
      titleInput.placeholder = "Entry title (e.g., Quiet evening chat)";
      titleInput.required = true;

      const moodSelect = document.createElement("select");
      moodSelect.className = "drawer-select";
      for (const tag of ALL_EMOTIONS) {
        const opt = document.createElement("option");
        opt.value = tag;
        opt.textContent = `Mood: ${EMOTION_META[tag].labelEn} · ${EMOTION_META[tag].labelTh}`;
        if (tag === (this.emotionProfile?.dominant ?? "joy")) opt.selected = true;
        moodSelect.appendChild(opt);
      }

      const contentArea = document.createElement("textarea");
      contentArea.className = "drawer-textarea";
      contentArea.rows = 3;
      contentArea.placeholder = "Write a memory or diary reflection...";
      contentArea.required = true;

      const formBtns = document.createElement("div");
      formBtns.className = "model-actions";
      const saveBtn = document.createElement("button");
      saveBtn.type = "submit";
      saveBtn.className = "btn-primary btn-sm";
      saveBtn.disabled = this.savingDiary;
      saveBtn.textContent = this.savingDiary ? "Saving..." : "Save Diary Entry";
      formBtns.appendChild(saveBtn);

      form.addEventListener("submit", (e) => {
        e.preventDefault();
        void this.handleAddDiaryEntry(
          titleInput.value,
          contentArea.value,
          moodSelect.value as EmotionTag
        );
      });

      form.append(titleInput, moodSelect, contentArea, formBtns);
      section.appendChild(form);
    }

    if (this.diaryEntries.length === 0) {
      const empty = document.createElement("p");
      empty.className = "empty-hint";
      const name = this.callbacks.getCompanionName?.() || "Coral";
      empty.textContent = `No personal diary entries for ${name} yet. Write one above or tap Sleep & Compact!`;
      section.appendChild(empty);
      return section;
    }

    for (const entry of this.diaryEntries) {
      const card = document.createElement("article");
      card.className = "diary-item";

      const meta = document.createElement("div");
      meta.className = "diary-meta";

      const leftBadges = document.createElement("div");
      leftBadges.className = "badge-cluster";

      const datePill = document.createElement("span");
      datePill.className = "pill pill-soft";
      datePill.textContent = entry.entryDate;

      const moodMeta = EMOTION_META[entry.mood] ?? EMOTION_META.neutral;
      const moodPill = document.createElement("span");
      moodPill.className = "pill";
      moodPill.style.background = moodMeta.pillBg;
      moodPill.style.color = moodMeta.accentColor;
      moodPill.textContent = moodMeta.labelEn;

      leftBadges.append(datePill, moodPill);

      const mergedBadge = document.createElement("span");
      mergedBadge.className = "pill pill-ok";
      mergedBadge.textContent = `merged ×${entry.mergedCount}`;

      meta.append(leftBadges, mergedBadge);

      const titleEl = document.createElement("div");
      titleEl.className = "diary-entry-title";
      titleEl.textContent = entry.title;

      const contentEl = document.createElement("p");
      contentEl.className = "diary-summary";
      contentEl.textContent = entry.content;

      const stamp = document.createElement("div");
      stamp.className = "diary-date";
      stamp.textContent = `${entry.source} · updated ${formatDateTime(entry.updatedAt)}`;

      card.append(meta, titleEl, contentEl, stamp);
      section.appendChild(card);
    }

    return section;
  }

  private renderPersonaBioCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card";

    const bio = this.personaBio;
    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = `Self-Bio (${bio?.displayName ?? (this.callbacks.getCompanionName?.() || "Coral")})`;
    const isoPill = document.createElement("span");
    isoPill.className = "pill pill-soft";
    isoPill.textContent = "Per-Character";
    head.append(h3, isoPill);

    const form = document.createElement("form");
    form.className = "profile-form";

    const traitsLabel = document.createElement("label");
    traitsLabel.className = "field-label";
    traitsLabel.textContent = "Traits";
    const traitsInput = document.createElement("input");
    traitsInput.type = "text";
    traitsInput.className = "drawer-input";
    traitsInput.value = bio?.traits ?? "";
    traitsLabel.appendChild(traitsInput);

    const reflLabel = document.createElement("label");
    reflLabel.className = "field-label";
    reflLabel.textContent = "Self-Reflection";
    const reflInput = document.createElement("textarea");
    reflInput.className = "drawer-textarea";
    reflInput.rows = 2;
    reflInput.value = bio?.selfReflection ?? "";
    reflLabel.appendChild(reflInput);

    const topicsLabel = document.createElement("label");
    topicsLabel.className = "field-label";
    topicsLabel.textContent = "Favorite Topics";
    const topicsInput = document.createElement("input");
    topicsInput.type = "text";
    topicsInput.className = "drawer-input";
    topicsInput.value = bio?.favoriteTopics ?? "";
    topicsLabel.appendChild(topicsInput);

    const row = document.createElement("div");
    row.className = "model-actions";
    const saveBtn = document.createElement("button");
    saveBtn.type = "submit";
    saveBtn.className = "btn-secondary btn-sm";
    saveBtn.disabled = this.savingBio;
    saveBtn.textContent = this.savingBio ? "Saving..." : "Save Self-Bio";
    row.appendChild(saveBtn);

    form.addEventListener("submit", (e) => {
      e.preventDefault();
      void this.handleSavePersonaBio(
        traitsInput.value,
        reflInput.value,
        topicsInput.value
      );
    });

    form.append(traitsLabel, reflLabel, topicsLabel, row);
    card.append(head, form);
    return card;
  }

  private renderUserProfileCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card";

    const prof = this.userProfile;
    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = "User Profile";
    const stamp = document.createElement("span");
    stamp.className = "pill pill-soft";
    stamp.textContent = prof?.updatedAt ? formatDateTime(prof.updatedAt) : "Shared";
    head.append(h3, stamp);

    const form = document.createElement("form");
    form.className = "profile-form";

    const rowTop = document.createElement("div");
    rowTop.className = "profile-grid-2";

    const nameLabel = document.createElement("label");
    nameLabel.className = "field-label";
    nameLabel.textContent = "Display Name";
    const nameInput = document.createElement("input");
    nameInput.type = "text";
    nameInput.className = "drawer-input";
    nameInput.value = prof?.displayName ?? "";
    nameLabel.appendChild(nameInput);

    const tzLabel = document.createElement("label");
    tzLabel.className = "field-label";
    tzLabel.textContent = "Timezone";
    const tzInput = document.createElement("input");
    tzInput.type = "text";
    tzInput.className = "drawer-input";
    tzInput.value = prof?.timezone ?? "";
    tzLabel.appendChild(tzInput);

    rowTop.append(nameLabel, tzLabel);

    const locLabel = document.createElement("label");
    locLabel.className = "field-label";
    locLabel.textContent = "Location";
    const locInput = document.createElement("input");
    locInput.type = "text";
    locInput.className = "drawer-input";
    locInput.value = prof?.location ?? "";
    locLabel.appendChild(locInput);

    const notesLabel = document.createElement("label");
    notesLabel.className = "field-label";
    notesLabel.textContent = "Notes & Preferences";
    const notesInput = document.createElement("textarea");
    notesInput.className = "drawer-textarea";
    notesInput.rows = 2;
    notesInput.value = prof?.notes ?? "";
    notesLabel.appendChild(notesInput);

    const actions = document.createElement("div");
    actions.className = "model-actions";
    const saveBtn = document.createElement("button");
    saveBtn.type = "submit";
    saveBtn.className = "btn-secondary btn-sm";
    saveBtn.disabled = this.savingUserProfile;
    saveBtn.textContent = this.savingUserProfile ? "Saving..." : "Save User Profile";
    actions.appendChild(saveBtn);

    form.addEventListener("submit", (e) => {
      e.preventDefault();
      void this.handleSaveUserProfile(
        nameInput.value,
        tzInput.value,
        locInput.value,
        notesInput.value
      );
    });

    form.append(rowTop, locLabel, notesLabel, actions);
    card.append(head, form);
    return card;
  }

  private renderDreamSection(): HTMLElement {
    const section = document.createElement("section");
    section.className = "drawer-section";

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = `Dream Reflections (${this.diaries.length})`;
    head.appendChild(h3);
    section.appendChild(head);

    if (this.diaries.length === 0) {
      const empty = document.createElement("p");
      empty.className = "empty-hint";
      const name = this.callbacks.getCompanionName?.() || "Coral";
      empty.textContent = `No dream reflections recorded yet. Chat with ${name} and tap Sleep & Compact above!`;
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
      const name = this.callbacks.getCompanionName?.() || "Coral";
      empty.textContent = `No remembered facts yet. Tell ${name} your favorite drink, hobbies, or name!`;
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

  render(): void {
    this.bodyEl.innerHTML = "";

    if (this.errorBanner) {
      const err = document.createElement("div");
      err.className = "error-banner";
      err.textContent = this.errorBanner;
      this.bodyEl.appendChild(err);
    }

    this.bodyEl.appendChild(this.renderBondCard());
    this.bodyEl.appendChild(this.renderEmotionProfileCard());
    this.bodyEl.appendChild(this.renderPersonalDiarySection());
    this.bodyEl.appendChild(this.renderPersonaBioCard());
    this.bodyEl.appendChild(this.renderUserProfileCard());
    this.bodyEl.appendChild(this.renderDreamSection());
    this.bodyEl.appendChild(this.renderFactsSection());
    this.bodyEl.appendChild(this.renderMigrationCard());
  }
}
