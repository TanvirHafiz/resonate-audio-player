const { invoke } = window.__TAURI__.core;
const { open } = window.__TAURI__.dialog;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow, currentMonitor } = window.__TAURI__.window;
const { PhysicalSize, PhysicalPosition } = window.__TAURI__.dpi;

const MINI_SIZE = { width: 300, height: 64 };
const MINI_MARGIN = 24;

// "free" or "pro" — set at startup from the backend build flag. Phase 3+
// features (mood/genre intelligence, recommendations, etc.) should check
// `appTier === "pro"` before rendering, since the free build's backend
// won't have those commands registered at all.
let appTier = "free";

let tracks = [];
let forYouTracks = [];
let displayTracks = [];
let viewMode = "library"; // "library" | "foryou"
let trackTags = {}; // { [track_id]: { mood: string[], genre: string[] } }
let nowPlaying = null;
let pollHandle = null;
let isSeeking = false;
let selectedIds = new Set();
let lastClickedIndex = null;
let lastRenderedActiveId = undefined;

function fmtTime(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}

function fmtDuration(seconds) {
  if (!Number.isFinite(seconds) || seconds <= 0) return "LIVE";
  return fmtTime(seconds);
}

const SOURCE_LABELS = { radio: "Internet Radio", podcast: "Podcast", url: "Stream" };

function trackSubtitle(t) {
  if (t.artist || t.album) {
    return [t.artist, t.album].filter(Boolean).join(" — ");
  }
  return SOURCE_LABELS[t.source_type] ?? "Stream";
}

async function refreshFolders() {
  const folders = await invoke("get_library_folders");
  const list = document.getElementById("folder-list");
  list.innerHTML = "";
  if (folders.length === 0) {
    list.innerHTML = `<div class="folder-chip">No folders added yet</div>`;
    return;
  }
  for (const f of folders) {
    const el = document.createElement("div");
    el.className = "folder-chip";
    el.textContent = f;
    el.title = f;
    list.appendChild(el);
  }
}

async function refreshTracks() {
  tracks = await invoke("get_tracks");
  renderTrackList();
  if (appTier === "pro") refreshTrackTags();
}

async function refreshTrackTags() {
  try {
    const rows = await invoke("get_all_track_tags");
    trackTags = {};
    for (const r of rows) trackTags[r.track_id] = { mood: r.mood, genre: r.genre };
    renderTrackList();
  } catch (e) {
    console.error("get_all_track_tags failed", e);
  }
}

function moodBadge(t) {
  if (appTier !== "pro") return "";
  const mood = trackTags[t.id]?.mood?.[0];
  return mood ? `<span class="mood-badge">${escapeHtml(mood)}</span>` : "";
}

function rowTooltipText(t) {
  let text = trackSubtitle(t);
  const tags = trackTags[t.id];
  if (appTier === "pro" && tags && (tags.mood.length || tags.genre.length)) {
    text += ` · ${[...tags.genre, ...tags.mood].join(", ")}`;
  }
  return text;
}

function renderTrackList() {
  const container = document.getElementById("track-list");
  container.innerHTML = "";
  displayTracks = viewMode === "foryou" ? forYouTracks : tracks;

  if (displayTracks.length === 0) {
    container.innerHTML =
      viewMode === "foryou"
        ? `<div class="empty-state">Nothing to recommend yet — play a few tracks first so Resonate can learn your taste.</div>`
        : `<div class="empty-state">No tracks yet. Add a folder, or drag audio files/folders anywhere in this window.</div>`;
    return;
  }

  const activeId = nowPlaying?.track?.id;

  displayTracks.forEach((t, index) => {
    const row = document.createElement("div");
    const classes = ["track-row"];
    if (t.id === activeId) classes.push("active");
    if (selectedIds.has(t.id)) classes.push("selected");
    row.className = classes.join(" ");
    row.innerHTML = `
      <span class="row-title">${escapeHtml(t.title)}</span>
      ${moodBadge(t)}
      <span class="row-duration mono">${fmtDuration(t.duration_seconds)}</span>
      <div class="row-actions">
        ${appTier === "pro" ? `<button class="row-similar" title="More like this">&#8776;</button>` : ""}
        <button class="row-remove" title="Remove from library">&#10005;</button>
      </div>
      <div class="row-tooltip">${escapeHtml(rowTooltipText(t))}</div>
    `;

    row.addEventListener("click", (e) => handleRowClick(e, t, index));
    row.querySelector(".row-remove").addEventListener("click", (e) => {
      e.stopPropagation();
      removeTracks([t.id]);
    });
    row.querySelector(".row-similar")?.addEventListener("click", (e) => {
      e.stopPropagation();
      showSimilarTracks(t);
    });

    container.appendChild(row);
  });

  updateRemoveButtonVisibility();
}

function handleRowClick(e, track, index) {
  if (e.ctrlKey || e.metaKey) {
    if (selectedIds.has(track.id)) {
      selectedIds.delete(track.id);
    } else {
      selectedIds.add(track.id);
    }
    lastClickedIndex = index;
    renderTrackList();
    return;
  }

  if (e.shiftKey && lastClickedIndex !== null) {
    const [start, end] = [lastClickedIndex, index].sort((a, b) => a - b);
    selectedIds = new Set(displayTracks.slice(start, end + 1).map((t) => t.id));
    renderTrackList();
    return;
  }

  selectedIds.clear();
  lastClickedIndex = index;
  playTrack(track.id);
}

function updateRemoveButtonVisibility() {
  document.getElementById("remove-selected-btn").classList.toggle("visible", selectedIds.size > 0);
}

async function removeTracks(ids) {
  if (!ids || ids.length === 0) return;
  await invoke("remove_tracks", { trackIds: ids });
  for (const id of ids) selectedIds.delete(id);
  await refreshTracks();
}

function escapeHtml(str) {
  const div = document.createElement("div");
  div.textContent = str ?? "";
  return div.innerHTML;
}

function setupViewTabs() {
  document.getElementById("view-tabs").addEventListener("click", async (e) => {
    const btn = e.target.closest(".tab-btn");
    if (!btn || btn.hidden) return;
    viewMode = btn.dataset.view;
    document.querySelectorAll(".tab-btn").forEach((b) => b.classList.toggle("active", b === btn));
    hideSimilarPanel();

    if (viewMode === "foryou") {
      try {
        forYouTracks = await invoke("for_you_queue", { limit: 40 });
      } catch (e) {
        console.error("for_you_queue failed", e);
        forYouTracks = [];
      }
    }
    renderTrackList();
  });
}

function hideSimilarPanel() {
  document.getElementById("similar-panel").hidden = true;
}

async function showSimilarTracks(seedTrack) {
  const panel = document.getElementById("similar-panel");
  const titleEl = document.getElementById("similar-title");
  const listEl = document.getElementById("similar-list");

  panel.hidden = false;
  titleEl.textContent = `Similar to ${seedTrack.title}`;
  listEl.innerHTML = `<div class="empty-state">Finding similar tracks…</div>`;

  let results = [];
  try {
    results = await invoke("more_like_this", { trackId: seedTrack.id, limit: 8 });
  } catch (e) {
    console.error("more_like_this failed", e);
  }

  if (results.length === 0) {
    listEl.innerHTML = `<div class="empty-state">No similar tracks found yet.</div>`;
    return;
  }

  listEl.innerHTML = "";
  for (const t of results) {
    const row = document.createElement("div");
    row.className = "track-row";
    row.innerHTML = `
      <span class="row-title">${escapeHtml(t.title)}</span>
      ${moodBadge(t)}
      <span class="row-duration mono">${fmtDuration(t.duration_seconds)}</span>
    `;
    row.addEventListener("click", () => playTrack(t.id));
    listEl.appendChild(row);
  }
}

async function playTrack(trackId) {
  const track = await invoke("play_track", { trackId });
  applyNowPlayingTrack(track, true);
  await pollNowPlaying();
}

function applyNowPlayingTrack(track) {
  if (nowPlaying) nowPlaying.track = track;
  renderTrackList();
}

async function togglePlayPause() {
  await invoke("toggle_play_pause");
  await pollNowPlaying();
}

async function nextTrack() {
  await invoke("next_track");
  await pollNowPlaying();
}

async function previousTrack() {
  await invoke("previous_track");
  await pollNowPlaying();
}

async function setVolume(value) {
  await invoke("set_volume", { volume: value / 100 });
}

function renderNowPlaying(state) {
  nowPlaying = state;

  const titleEl = document.getElementById("np-title");
  const subEl = document.getElementById("np-sub");
  const playBtn = document.getElementById("play-btn");
  const vu = document.getElementById("vu-meter");
  const currentEl = document.getElementById("time-current");
  const totalEl = document.getElementById("time-total");
  const fillEl = document.getElementById("scrubber-fill");
  const volumeSlider = document.getElementById("volume-slider");

  const titlebarTitleEl = document.getElementById("titlebar-title");
  const miniTitleEl = document.getElementById("mini-title");
  const miniPlayBtn = document.getElementById("mini-play-btn");

  if (state.track) {
    titleEl.textContent = state.track.title;
    subEl.textContent = trackSubtitle(state.track);
    titlebarTitleEl.textContent = `${state.track.title} — ${trackSubtitle(state.track)}`;
    miniTitleEl.textContent = state.track.title;
  } else {
    titleEl.textContent = "Nothing playing";
    subEl.textContent = "Pick a track from your library";
    titlebarTitleEl.textContent = "Resonate";
    miniTitleEl.textContent = "Nothing playing";
  }

  playBtn.textContent = state.is_playing ? "⏸" : "▶";
  miniPlayBtn.textContent = state.is_playing ? "⏸" : "▶";
  vu.classList.toggle("playing", state.is_playing);

  const duration = state.track?.duration_seconds ?? 0;
  currentEl.textContent = fmtTime(state.position_seconds);
  totalEl.textContent = fmtDuration(duration);
  const pct = duration > 0 ? Math.min(100, (state.position_seconds / duration) * 100) : 0;
  fillEl.style.width = `${pct}%`;

  if (!isSeeking && document.activeElement !== volumeSlider) {
    volumeSlider.value = Math.round(state.volume * 100);
  }

  const activeId = state.track?.id ?? null;
  if (activeId !== lastRenderedActiveId) {
    lastRenderedActiveId = activeId;
    renderTrackList();
  }
}

async function pollNowPlaying() {
  try {
    const state = await invoke("get_now_playing");
    renderNowPlaying(state);
  } catch (e) {
    console.error("get_now_playing failed", e);
  }
}

async function addFolder() {
  const selected = await open({ directory: true, multiple: false });
  if (!selected) return;
  const btn = document.getElementById("add-folder-btn");
  btn.disabled = true;
  btn.textContent = "Scanning…";
  try {
    await invoke("scan_folder", { folder: selected });
    await refreshFolders();
    await refreshTracks();
  } finally {
    btn.disabled = false;
    btn.textContent = "+ Folder";
  }
}

async function handleDroppedPaths(paths) {
  if (!paths || paths.length === 0) return;
  await invoke("add_files", { paths });
  await refreshTracks();
}

let streamType = "radio";

function setupStreamPanel() {
  const panel = document.getElementById("stream-panel");
  const urlInput = document.getElementById("stream-url");
  const titleInput = document.getElementById("stream-title");
  const fetchBtn = document.getElementById("stream-fetch-btn");
  const addBtn = document.getElementById("stream-add-btn");
  const episodeList = document.getElementById("episode-list");

  function applyType(type) {
    streamType = type;
    document.querySelectorAll(".type-btn").forEach((btn) => {
      btn.classList.toggle("active", btn.dataset.type === type);
    });
    const isPodcast = type === "podcast";
    fetchBtn.hidden = !isPodcast;
    addBtn.hidden = isPodcast;
    titleInput.hidden = isPodcast;
    episodeList.hidden = true;
    episodeList.innerHTML = "";
  }

  document.getElementById("add-stream-btn").addEventListener("click", () => {
    panel.hidden = !panel.hidden;
  });
  document.getElementById("stream-close-btn").addEventListener("click", () => {
    panel.hidden = true;
  });
  document.getElementById("stream-type-toggle").addEventListener("click", (e) => {
    const btn = e.target.closest(".type-btn");
    if (btn) applyType(btn.dataset.type);
  });

  addBtn.addEventListener("click", async () => {
    const url = urlInput.value.trim();
    if (!url) return;
    addBtn.disabled = true;
    try {
      await invoke("add_stream", { url, title: titleInput.value.trim(), sourceType: streamType });
      urlInput.value = "";
      titleInput.value = "";
      await refreshTracks();
    } finally {
      addBtn.disabled = false;
    }
  });

  fetchBtn.addEventListener("click", async () => {
    const feedUrl = urlInput.value.trim();
    if (!feedUrl) return;
    fetchBtn.disabled = true;
    fetchBtn.textContent = "Fetching…";
    try {
      const episodes = await invoke("fetch_podcast_feed", { feedUrl });
      renderEpisodes(episodes);
    } catch (e) {
      episodeList.hidden = false;
      episodeList.innerHTML = `<div class="empty-state">Couldn't fetch that feed.</div>`;
      console.error("fetch_podcast_feed failed", e);
    } finally {
      fetchBtn.disabled = false;
      fetchBtn.textContent = "Fetch episodes";
    }
  });

  function renderEpisodes(episodes) {
    episodeList.hidden = false;
    episodeList.innerHTML = "";
    if (episodes.length === 0) {
      episodeList.innerHTML = `<div class="empty-state">No episodes found in that feed.</div>`;
      return;
    }
    for (const ep of episodes) {
      const row = document.createElement("div");
      row.className = "episode-row";
      row.innerHTML = `
        <span class="episode-title">${escapeHtml(ep.title)}</span>
        <button class="episode-add-btn" title="Add episode">+</button>
      `;
      row.querySelector(".episode-add-btn").addEventListener("click", async (e) => {
        const btn = e.currentTarget;
        btn.disabled = true;
        await invoke("add_stream", { url: ep.url, title: ep.title, sourceType: "podcast" });
        btn.textContent = "✓";
        await refreshTracks();
      });
      episodeList.appendChild(row);
    }
  }

  applyType("radio");
}

function setupDragAndDrop() {
  const appEl = document.getElementById("app");

  listen("tauri://drag-enter", () => appEl.classList.add("dragging"));
  listen("tauri://drag-leave", () => appEl.classList.remove("dragging"));
  listen("tauri://drag-drop", async (event) => {
    appEl.classList.remove("dragging");
    await handleDroppedPaths(event.payload?.paths);
  });
}

function setupWindowControls() {
  const win = getCurrentWindow();
  document.getElementById("win-minimize").addEventListener("click", () => win.minimize());
  document.getElementById("win-close").addEventListener("click", () => win.close());
}

let savedWindowRect = null;
let isMini = false;

async function enterMiniMode() {
  const win = getCurrentWindow();
  const [size, position, monitor] = await Promise.all([
    win.outerSize(),
    win.outerPosition(),
    currentMonitor(),
  ]);
  savedWindowRect = { size, position };

  document.getElementById("window").classList.add("mini");
  isMini = true;

  const scale = monitor?.scaleFactor ?? 1;
  const miniW = Math.round(MINI_SIZE.width * scale);
  const miniH = Math.round(MINI_SIZE.height * scale);
  const margin = Math.round(MINI_MARGIN * scale);

  await win.setResizable(false);
  await win.setSizeConstraints({ minWidth: 1, minHeight: 1 });
  await win.setSize(new PhysicalSize(miniW, miniH));

  if (monitor) {
    const x = monitor.position.x + monitor.size.width - miniW - margin;
    const y = monitor.position.y + monitor.size.height - miniH - margin;
    await win.setPosition(new PhysicalPosition(x, y));
  }

  await win.setAlwaysOnTop(true);
}

async function exitMiniMode() {
  const win = getCurrentWindow();
  document.getElementById("window").classList.remove("mini");
  isMini = false;

  await win.setAlwaysOnTop(false);
  await win.setResizable(true);
  await win.setSizeConstraints({ minWidth: 820, minHeight: 560 });
  if (savedWindowRect) {
    await win.setSize(savedWindowRect.size);
    await win.setPosition(savedWindowRect.position);
  }
}

function setupMiniMode() {
  document.getElementById("mini-toggle-btn").addEventListener("click", async () => {
    try {
      await enterMiniMode();
    } catch (e) {
      document.getElementById("window").classList.remove("mini");
      const msg = `enterMiniMode failed: ${e?.message ?? e} ${e?.stack ?? ""}`;
      document.getElementById("titlebar-title").textContent = msg.slice(0, 80);
      console.error(msg, e);
      invoke("debug_log", { message: msg }).catch(() => {});
    }
  });
  document.getElementById("mini-restore-btn").addEventListener("click", async () => {
    try {
      await exitMiniMode();
    } catch (e) {
      console.error("exitMiniMode failed", e);
    }
  });
  document.getElementById("mini-play-btn").addEventListener("click", togglePlayPause);
  document.getElementById("mini-next-btn").addEventListener("click", nextTrack);
  document.getElementById("mini-prev-btn").addEventListener("click", previousTrack);
}

window.addEventListener("DOMContentLoaded", async () => {
  document.getElementById("add-folder-btn").addEventListener("click", addFolder);
  document.getElementById("play-btn").addEventListener("click", togglePlayPause);
  document.getElementById("next-btn").addEventListener("click", nextTrack);
  document.getElementById("prev-btn").addEventListener("click", previousTrack);

  const volumeSlider = document.getElementById("volume-slider");
  volumeSlider.addEventListener("input", (e) => {
    isSeeking = true;
    setVolume(Number(e.target.value));
  });
  volumeSlider.addEventListener("change", () => {
    isSeeking = false;
  });

  document.getElementById("remove-selected-btn").addEventListener("click", () => {
    removeTracks(Array.from(selectedIds));
  });

  window.addEventListener("keydown", (e) => {
    const tag = document.activeElement?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA") return;
    if ((e.key === "Delete" || e.key === "Backspace") && selectedIds.size > 0) {
      e.preventDefault();
      removeTracks(Array.from(selectedIds));
    }
  });

  setupWindowControls();
  setupMiniMode();
  setupDragAndDrop();
  setupStreamPanel();
  setupViewTabs();
  document.getElementById("similar-close-btn").addEventListener("click", hideSimilarPanel);

  try {
    appTier = await invoke("app_tier");
  } catch {
    appTier = "free";
  }
  document.body.dataset.tier = appTier;
  if (appTier === "pro") {
    document.getElementById("foryou-tab").hidden = false;
    setInterval(refreshTrackTags, 5000);
  }

  await refreshFolders();
  await refreshTracks();
  await pollNowPlaying();

  pollHandle = setInterval(pollNowPlaying, 500);
});
