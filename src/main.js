const { invoke } = window.__TAURI__.core;
const { open } = window.__TAURI__.dialog;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow, currentMonitor } = window.__TAURI__.window;
const { PhysicalSize, PhysicalPosition } = window.__TAURI__.dpi;
const { openUrl } = window.__TAURI__.opener;

const MINI_SIZE = { width: 300, height: 64 };
const MINI_MARGIN = 24;

// "free" or "pro" — set at startup from the backend build flag. Phase 3+
// features (mood/genre intelligence, recommendations, etc.) should check
// `appTier === "pro"` before rendering, since the free build's backend
// won't have those commands registered at all.
let appTier = "free";

let tracks = [];
let forYouTracks = [];
let playlistTracks = [];
let displayTracks = [];
let viewMode = "library"; // "library" | "foryou" | "playlist"
let trackTags = {}; // { [track_id]: { mood: string[], genre: string[] } }
let playlists = [];
let activePlaylistId = null;
let searchQuery = "";
let moodFilter = "";
let nowPlaying = null;
let pollHandle = null;
let isSeeking = false;
let isDraggingScrubber = false;
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
  renderFolderList(document.getElementById("folder-list"), folders);
  if (appTier === "pro") renderFolderList(document.getElementById("settings-folder-list"), folders);
}

function renderFolderList(container, folders) {
  container.innerHTML = "";
  if (folders.length === 0) {
    container.innerHTML = `<div class="folder-chip">No folders added yet</div>`;
    return;
  }
  for (const f of folders) {
    const el = document.createElement("div");
    el.className = "folder-chip";
    el.title = f;
    el.innerHTML = `<span class="folder-chip-path">${escapeHtml(f)}</span>`;
    if (appTier === "pro") {
      const removeBtn = document.createElement("button");
      removeBtn.className = "folder-chip-remove";
      removeBtn.title = "Remove folder and its tracks";
      removeBtn.textContent = "×";
      removeBtn.addEventListener("click", async () => {
        await invoke("remove_library_folder", { path: f });
        await refreshFolders();
        await refreshTracks();
      });
      el.appendChild(removeBtn);
    }
    container.appendChild(el);
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
    const moods = new Set();
    for (const r of rows) {
      trackTags[r.track_id] = { mood: r.mood, genre: r.genre };
      for (const m of r.mood) moods.add(m);
    }
    updateMoodFilterOptions(moods);
    renderTrackList();
  } catch (e) {
    console.error("get_all_track_tags failed", e);
  }
}

function updateMoodFilterOptions(moods) {
  const select = document.getElementById("mood-filter-select");
  const sorted = Array.from(moods).sort();
  const current = select.value;
  select.innerHTML =
    `<option value="">All moods</option>` +
    sorted.map((m) => `<option value="${escapeHtml(m)}">${escapeHtml(m)}</option>`).join("");
  if (sorted.includes(current)) select.value = current;
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

function matchesFilters(t) {
  if (searchQuery) {
    const q = searchQuery.toLowerCase();
    const haystack = `${t.title} ${t.artist} ${t.album}`.toLowerCase();
    if (!haystack.includes(q)) return false;
  }
  if (moodFilter && !trackTags[t.id]?.mood?.includes(moodFilter)) {
    return false;
  }
  return true;
}

function renderTrackList() {
  const container = document.getElementById("track-list");
  container.innerHTML = "";
  const baseTracks = viewMode === "foryou" ? forYouTracks : viewMode === "playlist" ? playlistTracks : tracks;
  displayTracks = searchQuery || moodFilter ? baseTracks.filter(matchesFilters) : baseTracks;

  if (displayTracks.length === 0) {
    if (baseTracks.length > 0) {
      container.innerHTML = `<div class="empty-state">No tracks match your search/filter.</div>`;
      return;
    }
    const emptyMessages = {
      foryou: `<div class="empty-state">Nothing to recommend yet — play a few tracks first so Resonate can learn your taste.</div>`,
      playlist: `<div class="empty-state">This playlist is empty. Browse Library and use the + button to add tracks.</div>`,
    };
    container.innerHTML =
      emptyMessages[viewMode] ??
      `<div class="empty-state">No tracks yet. Add a folder, or drag audio files/folders anywhere in this window.</div>`;
    return;
  }

  const activeId = nowPlaying?.track?.id;
  const showPlaylistAdd = appTier === "pro" && activePlaylistId && viewMode !== "playlist";

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
        ${showPlaylistAdd ? `<button class="row-playlist-add" title="Add to playlist">+</button>` : ""}
        <button class="row-remove" title="${viewMode === "playlist" ? "Remove from playlist" : "Remove from library"}">&#10005;</button>
      </div>
      <div class="row-tooltip">${escapeHtml(rowTooltipText(t))}</div>
    `;

    row.addEventListener("click", (e) => handleRowClick(e, t, index));
    row.querySelector(".row-remove").addEventListener("click", (e) => {
      e.stopPropagation();
      if (viewMode === "playlist") {
        removeFromActivePlaylist(t.id);
      } else {
        removeTracks([t.id]);
      }
    });
    row.querySelector(".row-similar")?.addEventListener("click", (e) => {
      e.stopPropagation();
      showSimilarTracks(t);
    });
    row.querySelector(".row-playlist-add")?.addEventListener("click", async (e) => {
      e.stopPropagation();
      await invoke("add_track_to_playlist", { playlistId: activePlaylistId, trackId: t.id });
      await refreshPlaylists();
    });

    container.appendChild(row);
  });

  updateRemoveButtonVisibility();
}

async function removeFromActivePlaylist(trackId) {
  if (!activePlaylistId) return;
  await invoke("remove_track_from_playlist", { playlistId: activePlaylistId, trackId });
  playlistTracks = await invoke("get_playlist_tracks", { playlistId: activePlaylistId });
  await refreshPlaylists();
  renderTrackList();
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

async function refreshPlaylists() {
  playlists = await invoke("list_playlists");
  renderPlaylistChips();
}

function renderPlaylistChips() {
  const container = document.getElementById("playlist-chips");
  container.innerHTML = "";
  for (const p of playlists) {
    const chip = document.createElement("button");
    chip.className = "playlist-chip" + (p.id === activePlaylistId ? " active" : "");
    chip.innerHTML = `<span>${escapeHtml(p.name)} (${p.track_count})</span><span class="playlist-chip-remove">&#10005;</span>`;
    chip.addEventListener("click", (e) => {
      if (e.target.closest(".playlist-chip-remove")) {
        e.stopPropagation();
        deletePlaylist(p.id);
        return;
      }
      selectPlaylist(p);
    });
    container.appendChild(chip);
  }
}

async function selectPlaylist(p) {
  activePlaylistId = p.id;
  viewMode = "playlist";
  document.querySelectorAll(".tab-btn").forEach((b) => b.classList.remove("active"));
  hideSimilarPanel();

  const banner = document.getElementById("playlist-banner");
  banner.hidden = false;
  document.getElementById("playlist-banner-text").textContent = `Adding to: ${p.name}`;

  playlistTracks = await invoke("get_playlist_tracks", { playlistId: p.id });
  renderPlaylistChips();
  renderTrackList();
}

async function deletePlaylist(playlistId) {
  await invoke("delete_playlist", { playlistId });
  if (activePlaylistId === playlistId) {
    clearActivePlaylist();
  }
  await refreshPlaylists();
}

function clearActivePlaylist() {
  activePlaylistId = null;
  document.getElementById("playlist-banner").hidden = true;
  if (viewMode === "playlist") {
    viewMode = "library";
    document.querySelector('.tab-btn[data-view="library"]').classList.add("active");
  }
  renderPlaylistChips();
  renderTrackList();
}

function setupPlaylists() {
  document.getElementById("new-playlist-btn").addEventListener("click", async () => {
    const name = window.prompt("Playlist name:");
    if (!name || !name.trim()) return;
    await invoke("create_playlist", { name: name.trim() });
    await refreshPlaylists();
  });
  document.getElementById("playlist-banner-clear").addEventListener("click", clearActivePlaylist);
}

function setupSearch() {
  document.getElementById("search-input").addEventListener("input", (e) => {
    searchQuery = e.target.value.trim();
    renderTrackList();
  });
  document.getElementById("mood-filter-select").addEventListener("change", (e) => {
    moodFilter = e.target.value;
    renderTrackList();
  });
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

function setScrubberVisual(pct) {
  document.getElementById("scrubber-fill").style.width = `${pct}%`;
  document.getElementById("scrubber-thumb").style.left = `${pct}%`;
}

function setupScrubber() {
  const track = document.getElementById("scrubber-track");

  function ratioFromEvent(e) {
    const rect = track.getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
  }

  function currentDuration() {
    return nowPlaying?.track?.duration_seconds ?? 0;
  }

  track.addEventListener("mousedown", (e) => {
    const duration = currentDuration();
    if (!(duration > 0)) return; // nothing seekable (nothing playing, or a live stream)
    isDraggingScrubber = true;
    track.classList.add("dragging");
    const ratio = ratioFromEvent(e);
    setScrubberVisual(ratio * 100);
    document.getElementById("time-current").textContent = fmtTime(ratio * duration);
  });

  window.addEventListener("mousemove", (e) => {
    if (!isDraggingScrubber) return;
    const duration = currentDuration();
    const ratio = ratioFromEvent(e);
    setScrubberVisual(ratio * 100);
    document.getElementById("time-current").textContent = fmtTime(ratio * duration);
  });

  window.addEventListener("mouseup", async (e) => {
    if (!isDraggingScrubber) return;
    isDraggingScrubber = false;
    track.classList.remove("dragging");
    const duration = currentDuration();
    const ratio = ratioFromEvent(e);
    try {
      await invoke("seek_to", { positionSeconds: ratio * duration });
    } catch (err) {
      console.error("seek_to failed", err);
    }
    await pollNowPlaying();
  });
}

function renderNowPlaying(state) {
  nowPlaying = state;

  const titleEl = document.getElementById("np-title");
  const subEl = document.getElementById("np-sub");
  const playBtn = document.getElementById("play-btn");
  const vu = document.getElementById("vu-meter");
  const currentEl = document.getElementById("time-current");
  const totalEl = document.getElementById("time-total");
  const volumeSlider = document.getElementById("volume-slider");

  const titlebarTitleEl = document.getElementById("titlebar-title");
  const miniTitleEl = document.getElementById("mini-title");
  const miniPlayBtn = document.getElementById("mini-play-btn");

  if (state.track) {
    titleEl.textContent = state.track.title;
    subEl.textContent = trackSubtitle(state.track);
    miniTitleEl.textContent = state.track.title;
  } else {
    titleEl.textContent = "Nothing playing";
    subEl.textContent = "Pick a track from your library";
    miniTitleEl.textContent = "Nothing playing";
  }

  // In the free build, the titlebar carries a persistent upsell link
  // instead of the track name (set once at startup) — don't overwrite it.
  if (appTier === "pro") {
    titlebarTitleEl.textContent = state.track
      ? `${state.track.title} — ${trackSubtitle(state.track)}`
      : "Resonate";
  }

  playBtn.textContent = state.is_playing ? "⏸" : "▶";
  miniPlayBtn.textContent = state.is_playing ? "⏸" : "▶";
  vu.classList.toggle("playing", state.is_playing);

  const canEnrich = Boolean(state.track?.artist);
  for (const id of ["lyrics-btn", "youtube-btn", "bio-btn"]) {
    document.getElementById(id).disabled = !canEnrich;
  }

  const duration = state.track?.duration_seconds ?? 0;
  totalEl.textContent = fmtDuration(duration);
  if (!isDraggingScrubber) {
    currentEl.textContent = fmtTime(state.position_seconds);
    const pct = duration > 0 ? Math.min(100, (state.position_seconds / duration) * 100) : 0;
    setScrubberVisual(pct);
  }

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

function setupSettings() {
  const settingsBtn = document.getElementById("settings-btn");
  const modal = document.getElementById("settings-modal");

  settingsBtn.addEventListener("click", async () => {
    if (appTier !== "pro") return;
    modal.hidden = false;
    await refreshFolders();
    await refreshOutputDevices();
  });
  document.getElementById("settings-close-btn").addEventListener("click", () => {
    modal.hidden = true;
  });
  modal.addEventListener("click", (e) => {
    if (e.target === modal) modal.hidden = true;
  });

  document.getElementById("output-device-select").addEventListener("change", async (e) => {
    try {
      await invoke("set_output_device", { deviceName: e.target.value });
    } catch (err) {
      console.error("set_output_device failed", err);
    }
  });
}

async function refreshOutputDevices() {
  const select = document.getElementById("output-device-select");
  try {
    const [devices, current] = await Promise.all([
      invoke("list_output_devices"),
      invoke("get_current_output_device"),
    ]);
    select.innerHTML = devices
      .map((d) => `<option value="${escapeHtml(d)}" ${d === current ? "selected" : ""}>${escapeHtml(d)}</option>`)
      .join("");
  } catch (e) {
    console.error("failed to load output devices", e);
  }
}

function showInfoModal(title, bodyHtml) {
  document.getElementById("info-modal-title").textContent = title;
  document.getElementById("info-modal-body").innerHTML = bodyHtml;
  document.getElementById("info-modal").hidden = false;
}

async function openExternal(url) {
  try {
    await openUrl(url);
  } catch (e) {
    console.error("openUrl failed", e);
  }
}

async function showLyrics() {
  const track = nowPlaying?.track;
  if (!track?.artist) return;
  showInfoModal(`Lyrics — ${track.title}`, `<div class="empty-state">Fetching lyrics…</div>`);
  try {
    const lyrics = await invoke("fetch_lyrics", { artist: track.artist, title: track.title });
    document.getElementById("info-modal-body").innerHTML = lyrics
      ? escapeHtml(lyrics)
      : `<div class="empty-state">No lyrics found for this track.</div>`;
  } catch (e) {
    document.getElementById("info-modal-body").innerHTML = `<div class="empty-state">Couldn't fetch lyrics.</div>`;
    console.error("fetch_lyrics failed", e);
  }
}

async function showBio() {
  const track = nowPlaying?.track;
  if (!track?.artist) return;
  showInfoModal(`About ${track.artist}`, `<div class="empty-state">Fetching bio…</div>`);
  try {
    const bio = await invoke("fetch_artist_bio", { artist: track.artist });
    if (!bio) {
      document.getElementById("info-modal-body").innerHTML = `<div class="empty-state">No bio found for this artist.</div>`;
      return;
    }
    const linkHtml = bio.source_url ? `<p><a href="#" id="bio-source-link">Read more on Wikipedia</a></p>` : "";
    document.getElementById("info-modal-body").innerHTML = `<p>${escapeHtml(bio.summary)}</p>${linkHtml}`;
    document.getElementById("bio-source-link")?.addEventListener("click", (e) => {
      e.preventDefault();
      openExternal(bio.source_url);
    });
  } catch (e) {
    document.getElementById("info-modal-body").innerHTML = `<div class="empty-state">Couldn't fetch bio.</div>`;
    console.error("fetch_artist_bio failed", e);
  }
}

function openYoutubeForCurrentTrack() {
  const track = nowPlaying?.track;
  if (!track) return;
  const query = encodeURIComponent(`${track.artist ?? ""} ${track.title}`.trim());
  openExternal(`https://www.youtube.com/results?search_query=${query}`);
}

function setupUpsell() {
  const titlebarTitleEl = document.getElementById("titlebar-title");
  titlebarTitleEl.textContent = "Free mode · click here for full version features";
  titlebarTitleEl.classList.add("upsell-link");
  // Prevent the titlebar's deep drag-region from swallowing clicks on this
  // (now interactive) span — see drag.js's isDragRegion handling.
  titlebarTitleEl.setAttribute("data-tauri-drag-region", "false");

  const modal = document.getElementById("upsell-modal");
  titlebarTitleEl.addEventListener("click", () => {
    modal.hidden = false;
  });
  document.getElementById("upsell-close-btn").addEventListener("click", () => {
    modal.hidden = true;
  });
  modal.addEventListener("click", (e) => {
    if (e.target === modal) modal.hidden = true;
  });
}

function setupEnrichment() {
  document.getElementById("lyrics-btn").addEventListener("click", showLyrics);
  document.getElementById("youtube-btn").addEventListener("click", openYoutubeForCurrentTrack);
  document.getElementById("bio-btn").addEventListener("click", showBio);

  const modal = document.getElementById("info-modal");
  document.getElementById("info-modal-close").addEventListener("click", () => {
    modal.hidden = true;
  });
  modal.addEventListener("click", (e) => {
    if (e.target === modal) modal.hidden = true;
  });
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
  setupScrubber();
  setupViewTabs();
  setupSettings();
  setupPlaylists();
  setupSearch();
  setupEnrichment();
  document.getElementById("similar-close-btn").addEventListener("click", hideSimilarPanel);

  try {
    appTier = await invoke("app_tier");
  } catch {
    appTier = "free";
  }
  document.body.dataset.tier = appTier;
  if (appTier === "pro") {
    document.getElementById("foryou-tab").hidden = false;
    document.getElementById("playlists-section").hidden = false;
    document.getElementById("search-row").hidden = false;
    document.getElementById("enrichment-row").hidden = false;
    setInterval(refreshTrackTags, 5000);
    await refreshPlaylists();
  } else {
    document.getElementById("settings-btn").hidden = true;
    setupUpsell();
  }

  await refreshFolders();
  await refreshTracks();
  await pollNowPlaying();

  pollHandle = setInterval(pollNowPlaying, 500);
});
