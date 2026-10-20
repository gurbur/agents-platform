'use strict';

// agents hub console (slice S11): the owner's chat console over the v1 API.
//
// The owner token lives in memory and, only when the user opts in, in this
// tab's sessionStorage; never in localStorage, cookies or the URL.
// localStorage holds view preferences only (chat style, last room, how many
// messages of each room were seen). Server data reaches the DOM as text
// nodes only: message bodies, names and intros are data, never markup.
// The console never moves a read marker.
//
// Slice S12 adds registering and deleting agents from the directory. The
// two one-time credentials of a registration live only in the text of the
// credential dialog while it is open and are wiped when it closes; they
// never reach storage, the URL or the console log.
//
// Slice S13 adds topic rooms (create, invite, remove, archive) and moves the
// owner's posting to the one send endpoint (API.messages, POST with
// `sender_id`). A reply sends `reply_to` only and lands in the original's
// room. The "조용히" toggle sends `wake: false`; it is per room visit and
// never stored.
//
// Slice S14 shows presence from the directory the page already refreshes:
// a colored dot (green online, red busy, grey offline) that also says the
// state in words, the busy note as text, and for offline agents how long
// ago their key was last seen. No new endpoint, polling or storage; the
// delivery pill stays a separate, owner-only signal.
//
// Slice S15 folds threads: the room timeline reads top-level messages
// (`top_level=true`) and shows "답글 n개 ›" under a root; new replies come
// from the same feed poll (every message after the cursor) and are folded
// into their root by `thread_id`. Replying (button or swipe) opens the
// thread panel, which reads `thread_id=<root>` and answers with `reply_to`
// only: the quoted message, or else the thread's last message. "나" may
// mute a thread it takes part in. A correction is a new message with
// `corrects` (own messages only), marked on both sides. No new timer or
// storage.
//
// Slice S16 adds reactions. The kind registry is read with the directory
// refresh (all kinds, so a disabled kind's chip still has its glyph; the
// bar offers the enabled ones). A message shows its reactions as chips
// (glyph and count, the names in the title, "나"'s own highlighted); the
// hover menu (or a long press on touch) holds the reaction bar. "나" reacts
// quietly: `sender_id` only, never `wake`. Opening a room takes the room's
// reaction cursor; the existing feed poll then asks for the changes after it
// and patches the chips on screen. A `claim` shows as "맡음: …", dimmed once
// the same message has `done`. The owner edits kinds in a dialog next to
// the directory. No new timer or storage.

const TOKEN_STORAGE_KEY = 'agents-platform.owner-token';
const PREF_STYLE = 'agents-console-style';
const PREF_ROOM = 'agents-console-room';
const PREF_SEEN = 'agents-console-seen';

const PAGE_SIZE = 50;
const POLL_PAGE_SIZE = 100;
const POLL_FEED_MS = 3000;
const POLL_ROOMS_MS = 10000;
const SEARCH_DEBOUNCE_MS = 350;
const GROUP_GAP_MS = 5 * 60 * 1000;
const DELIVERY_STALE_MS = 5000;
const INBOX_PAGE_SIZE = 20;
const OLDER_TRIGGER_PX = 120;
const BOTTOM_SLACK_PX = 48;
const MAX_POLL_PAGES = 5;
// The thread panel reads up to this many messages per page (the hub's max).
const THREAD_PAGE_SIZE = 100;
const MAX_JUMP_PAGES = 10;
const SNIPPET_CONCURRENCY = 4;
// Swipe-left-to-reply on touch: the drag claims the gesture at SWIPE_CLAIM_PX
// leftward (horizontal at least as far as vertical), follows the finger up to
// SWIPE_MAX_PX, then resists; releasing past SWIPE_REPLY_PX starts the reply.
// A finger held still for SWIPE_LONG_PRESS_MS is a long press, not a swipe.
const SWIPE_CLAIM_PX = 8;
const SWIPE_LONG_PRESS_MS = 450;
const SWIPE_REPLY_PX = 60;
const SWIPE_MAX_PX = 80;
const SWIPE_RESIST = 0.3;
const SWIPE_HAPTIC_MS = 10;
// platform-wire AVATAR_MAX_BYTES; the hub has the final word.
const AVATAR_MAX_BYTES = 512 * 1024;
// A body this long or with this many lines starts folded.
const FOLD_LINES = 4;
const FOLD_CHARS = 240;
const COLORS = ['#8fa5c2', '#a9bd92', '#e0a3b6', '#a7a3dc', '#d6b56e', '#8fc4bd', '#d79b84', '#b8aed0'];
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

const API = Object.freeze({
  owner: () => '/v1/owner',
  agents: () => '/v1/agents',
  agent: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}`,
  rooms: () => '/v1/rooms',
  messages: () => '/v1/messages',
  message: (messageId) => `/v1/messages/${encodeURIComponent(messageId)}`,
  roomMember: (roomId, agentId) => `/v1/rooms/${encodeURIComponent(roomId)}/members/${encodeURIComponent(agentId)}`,
  roomArchive: (roomId) => `/v1/rooms/${encodeURIComponent(roomId)}/archive`,
  profile: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/profile`,
  avatar: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/avatar`,
  receiver: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/receiver`,
  membership: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/membership/all`,
  delivery: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/delivery`,
  retry: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/delivery/retry`,
  inbox: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/inbox`,
  thread: (rootId) => `/v1/threads/${encodeURIComponent(rootId)}`,
  participation: (rootId) => `/v1/threads/${encodeURIComponent(rootId)}/participation`,
  reactionKinds: () => '/v1/reaction-kinds',
  reactionKind: (slug) => `/v1/reaction-kinds/${encodeURIComponent(slug)}`,
  reaction: (messageId, kind) => `/v1/messages/${encodeURIComponent(messageId)}/reactions/${encodeURIComponent(kind)}`,
  reactions: () => '/v1/reactions',
});

const GENDER_LABEL = { female: '여성형', male: '남성형', none: '없음' };
const AGE_LABEL = { child: '어린이', teen: '10대', '20s': '20대', '30s_40s': '30~40대', '50s_plus': '50대 이상', none: '없음' };
const STATE_LABEL = { idle: 'idle', pending: 'pending', blocked: 'blocked' };
const PRESENCE_LABEL = { online: '온라인', busy: '바쁨', offline: '오프라인' };
const JOINED_LABEL = { root: '시작한 사람', opener: '첫 답글', poster: '답글', mentioned: '지목됨', barged: '끼어듦' };
const NEVER_SEEN = '본 적 없음';
// The reaction kinds the console reads as "맡음" and its end (ADR-0014).
const CLAIM_KIND = 'claim';
const DONE_KIND = 'done';
const NONE_TEXT = '—';

// The single piece of mutable state. `session` changes on connect and
// logout, `feedSeq` on every room switch, so late responses are dropped.
const S = {
  token: '',
  session: 0,
  me: null,
  postAs: [],
  agents: new Map(),
  directorySignature: '',
  rooms: [],
  extraRooms: [],
  snippets: new Map(),
  seen: {},
  room: null,
  feedSeq: 0,
  msgs: [],
  feedLoaded: false,
  feedError: null,
  hasOlder: false,
  olderCursor: null,
  olderBusy: false,
  pollBusy: false,
  newFrom: null,
  q: '',
  // The highest event_seq of a reply the feed poll folded into a root.
  foldedSeq: '0',
  // The open thread panel (see openThread), or null.
  thread: null,
  // The message the room composer is correcting, or null.
  correcting: null,
  expanded: new Set(),
  quotes: new Map(),
  style: 'bubble',
  edit: false,
  profileError: null,
  tab: {},
  member: null,
  infoTab: 'settings',
  delivery: new Map(),
  actionError: null,
  receiverEdit: null,
  inbox: new Map(),
  avatars: new Map(),
  avatarBusy: new Set(),
  avatarTarget: null,
  sending: false,
  regBusy: false,
  credFor: null,
  delTarget: null,
  delBusy: false,
  pendingSend: null,
  sendError: null,
  quiet: false,
  roomBusy: false,
  roomError: null,
  mk: null,
  mkBusy: false,
  // Every reaction kind (owner view, `all=true`) in registry order.
  kinds: [],
  kindsSignature: '',
  // The current room's reaction feed cursor; null until it is known.
  reactCursor: null,
  // Reactions being sent, as `${message_id} ${kind}`.
  reactBusy: new Set(),
  // The last failed reaction: { messageId, failure }, or null.
  reactError: null,
  kindBusy: false,
  timers: [],
};

// ------------------------------------------------------------- DOM helpers

const $ = (id) => document.getElementById(id);
const app = $('app');
const SVG_NS = 'http://www.w3.org/2000/svg';

// Builds an element. `props`: class, text, data (dataset), bg and width
// (set through the CSSOM, which the CSP allows), anything else an attribute.
function h(tag, props, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (value === null || value === undefined || value === false) continue;
    if (key === 'class') node.className = value;
    else if (key === 'text') node.textContent = value;
    else if (key === 'data') Object.assign(node.dataset, value);
    else if (key === 'bg') node.style.background = value;
    else if (key === 'width') node.style.width = value;
    else node.setAttribute(key, value === true ? '' : String(value));
  }
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false || child === '') continue;
    node.append(child);
  }
  return node;
}

const ICONS = {
  hash: [['path', { d: 'M5 9h14M4 15h14M10 3 8 21M16 3l-2 18' }]],
  reply: [['path', { d: 'M9 14 4 9l5-5' }], ['path', { d: 'M4 9h10a6 6 0 0 1 6 6v4' }]],
  close: [['path', { d: 'M6 6l12 12M18 6 6 18' }]],
  back: [['path', { d: 'M15 18l-6-6 6-6' }]],
  logout: [['path', { d: 'M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3' }], ['path', { d: 'M10 17l-5-5 5-5M5 12h11' }]],
  topic: [['path', { d: 'M3 5h12v8H8l-5 3z' }], ['path', { d: 'M18 9h3v11l-4-3h-6v-1' }]],
  fix: [['path', { d: 'M4 20h4L19 9l-4-4L4 16z' }], ['path', { d: 'M13 7l4 4' }]],
};

function icon(name) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('class', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  for (const [tag, attrs] of ICONS[name]) {
    const part = document.createElementNS(SVG_NS, tag);
    for (const [key, value] of Object.entries(attrs)) part.setAttribute(key, value);
    svg.append(part);
  }
  return svg;
}

// --------------------------------------------------------------- utilities

function prefGet(key) {
  try {
    return window.localStorage.getItem(key);
  } catch (_) {
    return null;
  }
}

function prefSet(key, value) {
  try {
    window.localStorage.setItem(key, value);
  } catch (_) {
    // Storage may be unavailable (private mode); preferences stay in memory.
  }
}

function ulid() {
  let time = Date.now();
  let head = '';
  for (let index = 0; index < 10; index += 1) {
    head = CROCKFORD[time % 32] + head;
    time = Math.floor(time / 32);
  }
  const random = new Uint8Array(16);
  window.crypto.getRandomValues(random);
  let tail = '';
  for (const byte of random) tail += CROCKFORD[byte % 32];
  return head + tail;
}

const seqNum = (seq) => Number(seq);
const firstLine = (text) => String(text).split('\n')[0];
const failureText = (failure) => `${failure.code} — ${failure.message}`;

function clock(date) {
  const hours = date.getHours();
  const minutes = String(date.getMinutes()).padStart(2, '0');
  return `${hours < 12 ? '오전' : '오후'} ${hours % 12 || 12}:${minutes}`;
}

const shortClock = (date) => clock(date).replace(/^오[전후] /, '');
const stamp = (iso) => {
  const date = new Date(iso);
  return `${date.getFullYear()}.${date.getMonth() + 1}.${date.getDate()} ${clock(date)}`;
};
const dayKey = (date) => `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
const dayLabel = (date) => date.toLocaleDateString('ko-KR', { year: 'numeric', month: 'long', day: 'numeric', weekday: 'long' });

function relative(iso) {
  if (!iso) return '';
  const date = new Date(iso);
  const now = new Date();
  const minutes = Math.floor((now - date) / 60000);
  if (minutes < 1) return '방금';
  if (minutes < 60) return `${minutes}분`;
  if (dayKey(date) === dayKey(now)) return `${Math.floor(minutes / 60)}시간`;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (dayKey(date) === dayKey(yesterday)) return '어제';
  if (date.getFullYear() === now.getFullYear()) return `${date.getMonth() + 1}월 ${date.getDate()}일`;
  return `${date.getFullYear()}.${date.getMonth() + 1}.${date.getDate()}`;
}

// The receiver URL with all but the first three host characters masked,
// keeping the scheme, port and path.
function maskedUrl(raw) {
  try {
    const url = new URL(raw);
    const host = url.hostname;
    const masked = `${host.slice(0, Math.min(3, Math.max(0, host.length - 3)))}••••••`;
    return `${url.protocol}//${masked}${url.port ? `:${url.port}` : ''}${url.pathname}`;
  } catch (_) {
    return '••••••';
  }
}

// --------------------------------------------------------------------- API

class ApiFailure extends Error {
  constructor(status, code, message, requestId) {
    super(message);
    this.status = status;
    this.code = code;
    this.requestId = requestId;
  }
}

function buildUrl(path, params) {
  const url = new URL(path, window.location.origin);
  for (const [name, value] of Object.entries(params || {})) {
    if (value !== null && value !== undefined && value !== '') url.searchParams.set(name, value);
  }
  return url;
}

async function readJson(response) {
  const text = await response.text();
  if (text === '') return null;
  try {
    return JSON.parse(text);
  } catch (_) {
    return null;
  }
}

async function failureFrom(response) {
  const body = await readJson(response);
  const error = body && body.error;
  if (error && typeof error.code === 'string') {
    return new ApiFailure(response.status, error.code, String(error.message), String(error.request_id || ''));
  }
  const requestId = response.headers.get('x-request-id') || '';
  return new ApiFailure(response.status, 'unexpected_response', response.statusText || 'no error body', requestId);
}

// The only way this page talks to the server. `json` sends a JSON body,
// `blob` a raw one (an avatar); `raw` returns the Response itself.
async function request(method, path, options = {}) {
  const session = S.session;
  const headers = {};
  if (S.token) headers.Authorization = `Bearer ${S.token}`;
  let body;
  if (options.json !== undefined) {
    headers['Content-Type'] = 'application/json';
    body = JSON.stringify(options.json);
  } else if (options.blob !== undefined) {
    headers['Content-Type'] = options.contentType;
    body = options.blob;
  }
  let response;
  try {
    response = await fetch(buildUrl(path, options.params), {
      method,
      headers,
      body,
      cache: 'no-store',
      credentials: 'omit',
      redirect: 'error',
    });
  } catch (error) {
    throw new ApiFailure(0, 'network_error', String(error.message || error), '');
  }
  if (session !== S.session) throw new ApiFailure(0, 'stale_session', 'session changed', '');
  if (!response.ok) {
    const failure = await failureFrom(response);
    if (failure.status === 401 && S.token) logout(`${failure.status} ${failureText(failure)}`);
    throw failure;
  }
  if (options.raw) return response;
  return readJson(response);
}

function asFailure(error) {
  if (error instanceof ApiFailure) return error;
  return new ApiFailure(0, 'client_error', String(error), '');
}

// ------------------------------------------------------------------ agents

const agentOf = (id) => S.agents.get(id) || null;
const nameOf = (id) => (agentOf(id) && agentOf(id).name) || id;

function colorOf(id) {
  let hash = 0;
  for (const char of id) hash = (hash * 31 + char.codePointAt(0)) >>> 0;
  return COLORS[hash % COLORS.length];
}

function initialOf(id) {
  const first = Array.from(nameOf(id).trim())[0] || '?';
  return first.toLocaleUpperCase();
}

function av(id, size) {
  const node = h('span', { class: `av s${size}`, data: { av: id }, bg: colorOf(id), 'aria-hidden': 'true' });
  paintAvatar(node, id);
  return node;
}

function paintAvatar(node, id) {
  const cached = S.avatars.get(id);
  const agent = agentOf(id);
  if (cached && agent && agent.avatar && agent.avatar.sha256 === cached.sha) {
    node.replaceChildren(h('img', { src: cached.url, alt: '' }));
  } else {
    node.replaceChildren(initialOf(id));
  }
}

function repaintAvatars(id) {
  for (const node of document.querySelectorAll(`.av[data-av="${id}"]`)) paintAvatar(node, id);
}

function forgetAvatar(id) {
  const cached = S.avatars.get(id);
  if (!cached) return;
  URL.revokeObjectURL(cached.url);
  S.avatars.delete(id);
}

// Fetches the avatars whose directory sha256 differs from the cached copy.
async function ensureAvatars() {
  for (const agent of S.agents.values()) {
    const id = agent.agent_id;
    const cached = S.avatars.get(id);
    if (!agent.avatar) {
      if (cached) {
        forgetAvatar(id);
        repaintAvatars(id);
      }
      continue;
    }
    if ((cached && cached.sha === agent.avatar.sha256) || S.avatarBusy.has(id)) continue;
    S.avatarBusy.add(id);
    loadAvatar(id, agent.avatar.sha256).finally(() => S.avatarBusy.delete(id));
  }
}

async function loadAvatar(id, sha) {
  try {
    const response = await request('GET', API.avatar(id), { raw: true });
    const blob = await response.blob();
    forgetAvatar(id);
    S.avatars.set(id, { sha, url: URL.createObjectURL(blob) });
    repaintAvatars(id);
  } catch (_) {
    // The initial stays; the next directory refresh tries again.
  }
}

function pill(agent, status) {
  const state = (status && status.state) || (agent && agent.delivery && agent.delivery.state) || null;
  if (!state) return h('span', { class: 'pill none', text: NONE_TEXT });
  if (state === 'no_receiver') {
    const label = agent && agent.agent_id === S.me ? '대상 아님' : 'receiver 없음';
    return h('span', { class: 'pill none', text: label });
  }
  return h('span', { class: `pill ${state}`, text: STATE_LABEL[state] || state });
}

// How long ago `iso` was, coarsely: "방금 전", "n분 전", "n시간 전", "n일 전".
function ago(iso) {
  const minutes = Math.max(0, Math.floor((Date.now() - new Date(iso).getTime()) / 60000));
  if (minutes < 1) return '방금 전';
  if (minutes < 60) return `${minutes}분 전`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}시간 전`;
  return `${Math.floor(hours / 24)}일 전`;
}

// An agent's presence as shown: the state and the detail next to it (the
// busy note, or when an offline agent's key was last seen). `null` for an
// id the directory does not list.
function presenceOf(agent) {
  if (!agent) return null;
  const presence = agent.presence || {};
  const state = PRESENCE_LABEL[presence.state] ? presence.state : 'offline';
  let detail = '';
  if (state === 'busy') detail = presence.note || '';
  if (state === 'offline') detail = agent.last_seen_at ? ago(agent.last_seen_at) : NEVER_SEEN;
  const label = PRESENCE_LABEL[state];
  return { state, label, detail, title: detail ? `${label} · ${detail}` : label };
}

// The dot alone; its title and label say the state in words as well.
function dot(agent) {
  const shown = presenceOf(agent);
  if (!shown) return null;
  return h('span', { class: `dot ${shown.state}`, role: 'img', title: shown.title, 'aria-label': shown.title });
}

// Dot, state and detail on one line. The note goes in as text only.
function presenceLine(agent) {
  const shown = presenceOf(agent);
  if (!shown) return null;
  return h('span', { class: `presence ${shown.state}`, title: shown.title },
    h('span', { class: `dot ${shown.state}`, 'aria-hidden': 'true' }),
    h('span', { class: 'state', text: shown.label }),
    shown.detail ? h('span', { class: 'detail', text: shown.detail }) : null);
}

// An avatar with the presence dot on its corner.
function avDot(id, size) {
  return h('span', { class: 'avp' }, av(id, size), dot(agentOf(id)));
}

// ------------------------------------------------------------------- rooms

function allRooms() {
  const known = new Set(S.rooms.map((room) => room.room_id));
  return S.rooms.concat(S.extraRooms.filter((room) => !known.has(room.room_id)));
}

const currentRoom = () => allRooms().find((room) => room.room_id === S.room) || null;

function roomKind(room) {
  if (room.kind === 'all' || room.kind === 'topic') return room.kind;
  return S.me && room.members.includes(S.me) ? 'dm' : 'pair';
}

const isArchived = (room) => Boolean(room && room.archived_at);

// The owner writes as "나" into any room that is not archived.
function canWrite() {
  return Boolean(S.me) && !isArchived(currentRoom());
}

const peerOf = (room) => room.members.find((id) => id !== S.me) || room.members[0];

function roomTitle(room) {
  const kind = roomKind(room);
  if (kind === 'all') return 'all';
  if (kind === 'topic') return room.title || room.room_id;
  if (kind === 'dm') return nameOf(peerOf(room));
  return room.members.map(nameOf).join(' ↔ ');
}

function roomIcon(room, size) {
  const kind = roomKind(room);
  if (kind === 'all') return h('span', { class: 'hashbox' }, icon('hash'));
  if (kind === 'topic') return h('span', { class: 'hashbox' }, icon('topic'));
  if (kind === 'dm') return avDot(peerOf(room), size);
  return h('span', { class: 'pair' }, av(room.members[0], 20), av(room.members[1], 20));
}

function dmRoomId(a, b) {
  const [first, second] = [a, b].sort();
  return `dm:${first}:${second}`;
}

function unreadOf(room) {
  if (room.room_id === S.room) return 0;
  const seen = S.seen[room.room_id] || 0;
  return Math.max(0, room.message_count - seen);
}

function markSeen(room) {
  if (!room) return;
  const loaded = S.msgs.length;
  S.seen[room.room_id] = Math.max(S.seen[room.room_id] || 0, room.message_count, loaded);
  prefSet(PREF_SEEN, JSON.stringify(S.seen));
}

function renderRooms() {
  const query = $('roomQ').value.trim();
  const rooms = allRooms().filter((room) => !query || roomTitle(room).includes(query));
  const topics = rooms.filter((room) => roomKind(room) === 'topic');
  const sections = [
    ['채널', rooms.filter((room) => roomKind(room) === 'all')],
    ['주제 방', topics.filter((room) => !isArchived(room)).concat(topics.filter(isArchived))],
    ['내 DM', rooms.filter((room) => roomKind(room) === 'dm')],
    ['에이전트끼리', rooms.filter((room) => roomKind(room) === 'pair')],
  ];
  const nodes = [];
  for (const [label, list] of sections) {
    if (list.length === 0) continue;
    nodes.push(h('div', { class: 'sec', text: label }));
    for (const room of list) nodes.push(roomRow(room));
  }
  if (nodes.length === 0) nodes.push(h('div', { class: 'empty', text: '맞는 방이 없습니다' }));
  $('rooms').replaceChildren(...nodes);
}

function roomRow(room) {
  const unread = unreadOf(room);
  const archived = isArchived(room);
  return h('button', { class: `room${archived ? ' archived' : ''}`, data: { act: 'room', id: room.room_id }, 'aria-current': String(room.room_id === S.room) },
    h('span', { class: 'ic' }, roomIcon(room, 32)),
    h('span', { class: 'nm' }, roomTitle(room), archived ? h('span', { class: 'tag', text: '보관됨' }) : null),
    h('span', { class: 'tm', text: relative(room.last_message_at) }),
    h('span', { class: 'sn', text: snippetText(room) }),
    unread > 0 ? h('span', { class: 'badge', text: String(unread) }) : h('span'));
}

function snippetText(room) {
  const snippet = S.snippets.get(room.room_id);
  if (!room.last_event_seq) return '아직 메시지가 없습니다';
  if (!snippet) return '';
  const who = roomKind(room) === 'dm'
    ? (snippet.sender === S.me ? '나: ' : '')
    : `${snippet.sender === S.me ? '나' : nameOf(snippet.sender)}: `;
  return who + firstLine(snippet.body);
}

// The last message of every room whose newest event_seq moved.
async function loadSnippets() {
  const stale = allRooms().filter((room) => room.last_event_seq
    && (S.snippets.get(room.room_id) || {}).seq !== room.last_event_seq);
  for (let start = 0; start < stale.length; start += SNIPPET_CONCURRENCY) {
    const chunk = stale.slice(start, start + SNIPPET_CONCURRENCY);
    await Promise.all(chunk.map(async (room) => {
      try {
        const page = await request('GET', API.messages(), { params: { room_id: room.room_id, limit: 1 } });
        const last = page.items[0];
        if (last) S.snippets.set(room.room_id, { seq: last.event_seq, sender: last.sender_id, body: last.body });
      } catch (_) {
        // Keep the old snippet; the next refresh retries.
      }
    }));
  }
  renderRooms();
}

function rememberSnippet(message) {
  const known = S.snippets.get(message.room_id);
  if (known && seqNum(known.seq) >= seqNum(message.event_seq)) return;
  S.snippets.set(message.room_id, { seq: message.event_seq, sender: message.sender_id, body: message.body });
}

// ----------------------------------------------------------------- top bar

function renderTop() {
  const room = currentRoom();
  if (!room) {
    $('ttl').replaceChildren();
    return;
  }
  const kind = roomKind(room);
  const subs = {
    all: `멤버 ${room.members.length}명 · 모두에게 전달`,
    topic: isArchived(room) ? `${room.room_id} · 보관됨 · 읽기만` : `${room.room_id} · 멤버 ${room.members.length}명`,
    dm: [`@${peerOf(room)} · 나와의 DM`, (presenceOf(agentOf(peerOf(room))) || {}).title].filter(Boolean).join(' · '),
    pair: `에이전트끼리 · ${room.members.join(' ↔ ')}`,
  };
  const sub = subs[kind];
  const glyph = { all: 'hash', topic: 'topic' }[kind];
  $('ttl').replaceChildren(
    glyph ? icon(glyph) : roomIcon(room, 28),
    h('b', { text: roomTitle(room) }),
    h('span', { class: 'sub', text: sub }));
  $('infoBtn').classList.toggle('on', !app.classList.contains('info-off') && !S.thread);
}

function renderStyle() {
  app.classList.toggle('style-bubble', S.style === 'bubble');
  for (const button of document.querySelectorAll('.mode button')) {
    button.setAttribute('aria-pressed', String(button.dataset.style === S.style));
  }
  $('modeBtn').classList.toggle('on', S.style === 'bubble');
  $('modeBtn').setAttribute('aria-label', S.style === 'bubble'
    ? '보기 방식: 말풍선형 (누르면 디스코드형)'
    : '보기 방식: 디스코드형 (누르면 말풍선형)');
}

function renderMe() {
  const pillText = S.me ? `소유자 · ${nameOf(S.me)}` : '소유자';
  $('ownerPill').textContent = pillText;
  const logout = h('button', { class: 'btn icon', data: { act: 'logout' }, 'aria-label': '로그아웃', title: '로그아웃' }, icon('logout'));
  if (!S.me) {
    $('me').replaceChildren(h('span', { class: 'who' }, '소유자', h('small', { text: 'post_as 없음 · 읽기 전용' })), logout);
    return;
  }
  $('me').replaceChildren(av(S.me, 32), h('span', { class: 'who' }, nameOf(S.me), h('small', { text: `${S.me} · 소유자` })), logout);
}

// -------------------------------------------------------------------- feed

const feed = $('feed');
const threadFeed = $('threadFeed');
const findMessage = (id) => S.msgs.find((message) => message.message_id === id) || null;
const findThreadMessage = (id) => (S.thread && S.thread.msgs.find((message) => message.message_id === id)) || null;

function quoteTarget(id) {
  const loaded = findMessage(id) || findThreadMessage(id);
  if (loaded) return loaded;
  const cached = S.quotes.get(id);
  if (cached) return cached;
  fetchQuote(id);
  return null;
}

async function fetchQuote(id) {
  S.quotes.set(id, { pending: true });
  const seq = S.feedSeq;
  try {
    S.quotes.set(id, await request('GET', API.message(id)));
  } catch (_) {
    S.quotes.set(id, { missing: true });
  }
  if (seq === S.feedSeq) rerenderFeed();
}

function highlighted(text) {
  if (!S.q) return [text];
  const parts = String(text).split(S.q);
  const nodes = [];
  parts.forEach((part, index) => {
    if (index > 0) nodes.push(h('mark', { text: S.q }));
    if (part) nodes.push(part);
  });
  return nodes;
}

function foldable(message) {
  return message.body.split('\n').length > FOLD_LINES || message.body.length > FOLD_CHARS;
}

function textBody(message) {
  const fold = foldable(message);
  const open = S.expanded.has(message.message_id);
  return [
    h('div', { class: `text${fold && !open ? ' clamp' : ''}` }, highlighted(message.body)),
    fold ? h('button', { class: 'more', data: { act: 'more', k: message.message_id }, text: open ? '접기' : '더 보기' }) : null,
  ];
}

// The hover menu (and long press) of a message: answer it, and correct it
// when "나" wrote it. `where` is 'feed' or 'thread', the list it is in.
function actions(message, where) {
  if (!canWrite()) return null;
  const prefix = where === 'thread' ? 'thread-' : '';
  return h('div', { class: 'actions' },
    reactionBar(message),
    h('button', { data: { act: `${prefix}reply`, k: message.message_id } }, icon('reply'), '답장'),
    message.sender_id === S.me
      ? h('button', { data: { act: `${prefix}correct`, k: message.message_id } }, icon('fix'), '정정')
      : null);
}

function isBargeIn(message, room) {
  return room.kind === 'dm' && !room.members.includes(message.sender_id);
}

function quoteParts(target) {
  if (!target || target.pending) return { who: '…', text: '원문을 불러오는 중' };
  if (target.missing) return { who: '', text: '원문을 찾을 수 없습니다' };
  return { who: nameOf(target.sender_id), text: firstLine(target.body), sender: target.sender_id };
}

// Every message on screen (timeline and thread panel) that corrects
// another, by the corrected message's id.
function correctionsOnScreen() {
  const found = new Map();
  const shown = S.msgs.concat(S.thread ? S.thread.msgs : []);
  for (const message of shown) {
    if (message.corrects && !found.has(message.corrects)) found.set(message.corrects, message.message_id);
  }
  return found;
}

// A quote is shown for a reply to anything but the thread's root, which
// the thread panel already shows on top.
function quoteOf(message, where) {
  if (!message.reply_to) return null;
  if (where === 'thread' && S.thread && message.reply_to === S.thread.root) return null;
  return quoteParts(quoteTarget(message.reply_to));
}

// The correction marks of a message: "정정" with a way to the original,
// and "정정됨 → 보기" on an original whose correction is on screen.
function correctionMarks(message, corrections) {
  const marks = [];
  if (message.corrects) {
    marks.push(h('span', { class: 'tag', text: '정정' }),
      h('button', { class: 'fixlink', data: { act: 'jump', k: message.corrects }, text: '원 글 보기' }));
  }
  const fixedBy = corrections.get(message.message_id);
  if (fixedBy) marks.push(h('button', { class: 'fixlink', data: { act: 'jump', k: fixedBy }, text: '정정됨 → 보기' }));
  return marks.length > 0 ? h('div', { class: 'fix' }, marks) : null;
}

// "답글 n개 ›" under a root of the timeline.
function repliesButton(message, where) {
  const summary = message.thread_summary;
  if (where !== 'feed' || !summary || summary.reply_count === 0) return null;
  return h('button', { class: 'replies', data: { act: 'thread', k: message.message_id }, text: `답글 ${summary.reply_count}개 ›` });
}

function flatMessage(item, room, where, corrections) {
  const message = item.message;
  const quote = quoteOf(message, where);
  const date = new Date(message.created_at);
  const nodes = [];
  if (quote) {
    nodes.push(h('button', { class: 'quote', data: { act: 'jump', k: message.reply_to } },
      quote.sender ? av(quote.sender, 16) : null, quote.who ? h('b', { text: quote.who }) : null, h('span', { text: quote.text })));
  }
  const content = [correctionMarks(message, corrections), textBody(message), reactionChips(message), repliesButton(message, where)];
  if (item.head) {
    const name = h('span', { class: 'name', text: nameOf(message.sender_id), bg: null });
    name.style.color = colorOf(message.sender_id);
    nodes.push(av(message.sender_id, 36), h('div', null,
      h('div', { class: 'meta' }, name,
        isBargeIn(message, room) ? h('span', { class: 'tag', text: '끼어듦' }) : null,
        h('time', { datetime: message.created_at, text: clock(date) })),
      content));
  } else {
    nodes.push(h('span', { class: 'gut', text: shortClock(date) }), h('div', null, content));
  }
  nodes.push(actions(message, where));
  return h('div', { class: `msg${item.head ? ' head' : ''}`, id: rowId(where, message.message_id) }, nodes);
}

function bubbleMessage(item, room, where, corrections) {
  const message = item.message;
  const mine = message.sender_id === S.me;
  const quote = quoteOf(message, where);
  const bq = quote ? h('button', { class: 'bq', data: { act: 'jump', k: message.reply_to } },
    quote.who ? h('b', { text: quote.who }) : null, h('span', { text: quote.text })) : null;
  let top = null;
  if (item.head && mine && isBargeIn(message, room)) top = h('div', { class: 'b-name' }, h('span', { class: 'tag', text: '끼어듦' }));
  if (item.head && !mine) top = h('div', { class: 'b-name', text: nameOf(message.sender_id) });
  const classes = ['b-row', mine ? 'mine' : 'other', item.head ? 'head' : '', item.tail ? 'tail' : ''].filter(Boolean).join(' ');
  return h('div', { class: classes, id: rowId(where, message.message_id) },
    mine ? null : h('div', { class: 'b-av' }, item.head ? av(message.sender_id, 36) : null),
    h('div', { class: 'b-col' }, top,
      h('div', { class: 'b-line' },
        h('div', { class: 'bubble' }, bq, correctionMarks(message, corrections), textBody(message)),
        item.tail ? h('time', { class: 'b-time', datetime: message.created_at, text: clock(new Date(message.created_at)) }) : null,
        actions(message, where)),
      reactionChips(message),
      repliesButton(message, where)));
}

// --------------------------------------------------------------- reactions

const kindOf = (slug) => S.kinds.find((kind) => kind.slug === slug) || null;
const glyphOf = (slug) => (kindOf(slug) ? kindOf(slug).glyph : slug);
const labelOf = (slug) => (kindOf(slug) ? kindOf(slug).label : slug);
const reactionsOf = (message) => message.reactions || [];
const reactedByMe = (entry) => Boolean(S.me && entry && entry.agents.includes(S.me));

// The registry (every kind: the owner's view). Returns whether it changed.
async function loadKinds() {
  let list;
  try {
    list = await request('GET', API.reactionKinds(), { params: { all: 'true' } });
  } catch (_) {
    return false;
  }
  S.kinds = list.kinds;
  const signature = JSON.stringify(S.kinds);
  if (signature === S.kindsSignature) return false;
  S.kindsSignature = signature;
  if ($('kinds').open) renderKinds();
  return true;
}

// The reaction bar of the hover menu: the enabled kinds, "나"'s own lit.
function reactionBar(message) {
  const kinds = S.kinds.filter((kind) => kind.enabled);
  if (!S.me || kinds.length === 0) return null;
  return h('span', { class: 'rbar', role: 'group', 'aria-label': '반응' }, kinds.map((kind) => {
    const mine = reactedByMe(reactionsOf(message).find((entry) => entry.kind === kind.slug));
    return h('button', {
      class: mine ? 'on' : null, title: kind.label, 'aria-label': kind.label, 'aria-pressed': String(mine),
      data: { act: 'react', k: message.message_id, kind: kind.slug },
    }, kind.glyph);
  }));
}

// "맡음: <names>" when the message has a `claim`; dimmed once it has `done`.
function claimBadge(message) {
  const list = reactionsOf(message);
  const claim = list.find((entry) => entry.kind === CLAIM_KIND);
  if (!claim) return null;
  const done = list.some((entry) => entry.kind === DONE_KIND);
  return h('span', {
    class: `claim${done ? ' done' : ''}`,
    title: done ? '맡음 — 처리 끝' : '맡음',
    text: `맡음: ${claim.agents.map(nameOf).join(', ')}`,
  });
}

// The chips under a message: glyph and count, the names in the title.
// "나"'s chip is highlighted and toggles it.
function reactionChips(message) {
  const list = reactionsOf(message);
  const failed = S.reactError && S.reactError.messageId === message.message_id ? S.reactError.failure : null;
  if (list.length === 0 && !failed) return null;
  const writable = canWrite() && Boolean(S.me);
  return h('div', { class: 'reacts' },
    claimBadge(message),
    list.map((entry) => {
      const mine = reactedByMe(entry);
      return h('button', {
        class: `rchip${mine ? ' mine' : ''}`,
        title: `${labelOf(entry.kind)} — ${entry.agents.map(nameOf).join(', ')}`,
        'aria-pressed': String(mine), disabled: !writable,
        data: { act: 'react', k: message.message_id, kind: entry.kind },
      }, h('span', { class: 'g', text: glyphOf(entry.kind) }), h('span', { class: 'n', text: String(entry.count) }));
    }),
    failed ? h('span', { class: 'err', role: 'alert', text: failureText(failed) }) : null);
}

// Registry order: `sort`, then slug; an unknown kind goes last.
function sortReactions(list) {
  const rank = (slug) => (kindOf(slug) ? kindOf(slug).sort : Number.MAX_SAFE_INTEGER);
  list.sort((a, b) => rank(a.kind) - rank(b.kind) || (a.kind < b.kind ? -1 : Number(a.kind > b.kind)));
}

// One addition or removal by `agentId` on a message's summary. Applying a
// change twice changes nothing; returns whether this one did.
function applyReaction(message, kind, agentId, removed) {
  if (!message.reactions) message.reactions = [];
  const list = message.reactions;
  let entry = list.find((candidate) => candidate.kind === kind);
  const present = Boolean(entry && entry.agents.includes(agentId));
  if (present !== removed) return false;
  if (removed) {
    entry.agents = entry.agents.filter((id) => id !== agentId);
    entry.count = entry.agents.length;
    if (entry.count === 0) list.splice(list.indexOf(entry), 1);
    return true;
  }
  if (!entry) {
    entry = { kind, count: 0, agents: [] };
    list.push(entry);
  }
  entry.agents = entry.agents.concat([agentId]).sort();
  entry.count = entry.agents.length;
  sortReactions(list);
  return true;
}

// Every loaded copy of a message: the timeline's and the thread panel's.
function copiesOf(id) {
  const copies = new Set(S.msgs.filter((message) => message.message_id === id));
  if (S.thread) for (const message of S.thread.msgs) if (message.message_id === id) copies.add(message);
  return Array.from(copies);
}

// Patches the copies of a message; says which lists show it.
function patchReactions(id, patch) {
  let changed = false;
  for (const message of copiesOf(id)) changed = patch(message) || changed;
  return {
    feedChanged: changed && S.msgs.some((message) => message.message_id === id),
    threadChanged: changed && Boolean(S.thread && S.thread.msgs.some((message) => message.message_id === id)),
  };
}

// The room's current reaction cursor: changes after it are the poll's.
async function loadReactionCursor() {
  const seq = S.feedSeq;
  try {
    const page = await request('GET', API.reactions(), { params: { room_id: S.room } });
    if (seq === S.feedSeq) S.reactCursor = page.next_after_change_seq;
  } catch (_) {
    // The next poll asks again.
  }
}

// The reaction changes of the room after the cursor (called by the feed
// poll, never on a timer of its own). Null when the room changed meanwhile.
async function pollReactions(seq) {
  const changed = { feedChanged: false, threadChanged: false };
  if (S.reactCursor === null) {
    await loadReactionCursor();
    return changed;
  }
  for (let page = 0; page < MAX_POLL_PAGES; page += 1) {
    const result = await request('GET', API.reactions(), { params: { room_id: S.room, after_change_seq: S.reactCursor, limit: POLL_PAGE_SIZE } });
    if (seq !== S.feedSeq) return null;
    for (const change of result.items) {
      const patched = patchReactions(change.message_id, (message) => applyReaction(message, change.kind, change.agent_id, change.removed));
      changed.feedChanged = changed.feedChanged || patched.feedChanged;
      changed.threadChanged = changed.threadChanged || patched.threadChanged;
    }
    S.reactCursor = result.next_after_change_seq;
    if (!result.has_more) break;
  }
  return changed;
}

function rerenderReactions(changed) {
  if (changed.feedChanged) rerenderFeed();
  if (changed.threadChanged) {
    rerenderThread();
    renderThreadMeta();
  }
}

// Adds "나"'s reaction, or removes it when "나" already has it. Quiet:
// the owner names "나" and nothing else.
async function toggleReaction(messageId, kind) {
  if (!S.me || !canWrite()) return;
  const key = `${messageId} ${kind}`;
  if (S.reactBusy.has(key)) return;
  const [message] = copiesOf(messageId);
  const mine = Boolean(message) && reactedByMe(reactionsOf(message).find((entry) => entry.kind === kind));
  S.reactBusy.add(key);
  S.reactError = null;
  let changed;
  try {
    if (mine) {
      await request('DELETE', API.reaction(messageId, kind), { json: { sender_id: S.me } });
      changed = patchReactions(messageId, (copy) => applyReaction(copy, kind, S.me, true));
    } else {
      const reacted = await request('PUT', API.reaction(messageId, kind), { json: { sender_id: S.me } });
      changed = patchReactions(messageId, (copy) => {
        copy.reactions = reacted.reactions;
        return true;
      });
    }
  } catch (error) {
    S.reactError = { messageId, failure: asFailure(error) };
    changed = { feedChanged: true, threadChanged: Boolean(S.thread) };
  } finally {
    S.reactBusy.delete(key);
  }
  rerenderReactions(changed);
}

// A long press on touch opens a message's menu (the hover menu elsewhere).
function openActions(row) {
  closeActions();
  if (row.isConnected) row.classList.add('acts-open');
}

function closeActions() {
  for (const open of document.querySelectorAll('.acts-open')) open.classList.remove('acts-open');
}

// Android fires contextmenu on a long press: the menu, not the browser's.
function onRowContextMenu(event) {
  if (!swipe || swipe.claimed) return;
  event.preventDefault();
  const { row } = swipe;
  swipe = null;
  openActions(row);
  swallowNextClick();
}

// ------------------------------------------------------- reaction kinds

function renderKinds() {
  $('kindsCount').textContent = `종류 ${S.kinds.length}`;
  $('kindsBody').replaceChildren(...S.kinds.map((kind) => h('tr', { class: kind.enabled ? null : 'off' },
    h('td', { class: 'glyph', text: kind.glyph }),
    h('td', null, h('span', { class: 'id', text: kind.slug })),
    h('td', { text: kind.label }),
    h('td', { text: String(kind.sort) }),
    h('td', null, h('button', {
      class: 'switch', role: 'switch', 'aria-checked': String(kind.enabled), 'aria-label': `${kind.slug} 켜짐`,
      data: { act: 'kind-toggle', id: kind.slug }, disabled: S.kindBusy,
    })),
    h('td', { class: 'act' }, h('button', { class: 'btn sm', data: { act: 'kind-edit', id: kind.slug }, text: '수정' })))));
}

function openKinds() {
  resetKindForm();
  renderKinds();
  $('kinds').showModal();
  $('kinds').focus();
  loadKinds().then((changed) => {
    if (!changed) renderKinds();
  });
}

function resetKindForm() {
  $('kindForm').reset();
  $('kindSlug').readOnly = false;
  $('kindEnabled').checked = true;
  $('kindFormTitle').textContent = '새 종류';
  $('kindGo').textContent = '추가';
  showFormError('kindError', null);
}

function editKind(slug) {
  const kind = kindOf(slug);
  if (!kind) return;
  $('kindSlug').value = kind.slug;
  $('kindSlug').readOnly = true;
  $('kindGlyph').value = kind.glyph;
  $('kindLabel').value = kind.label;
  $('kindSort').value = String(kind.sort);
  $('kindEnabled').checked = kind.enabled;
  $('kindFormTitle').textContent = `${kind.slug} 수정`;
  $('kindGo').textContent = '저장';
  showFormError('kindError', null);
  $('kindGlyph').focus();
}

// The registry changed: the dialog, the bars and the chips follow.
async function afterKindChange() {
  await loadKinds();
  renderKinds();
  rerenderFeed();
  rerenderThread();
}

async function putKind(slug, body) {
  if (S.kindBusy) return false;
  S.kindBusy = true;
  $('kindGo').disabled = true;
  showFormError('kindError', null);
  try {
    await request('PUT', API.reactionKind(slug), { json: body });
    return true;
  } catch (error) {
    showFormError('kindError', failureText(asFailure(error)));
    return false;
  } finally {
    S.kindBusy = false;
    $('kindGo').disabled = false;
  }
}

// The glyph goes as typed (the hub refuses spaces rather than trimming); a
// `sort` that is no integer goes as text so the hub names the field.
async function submitKind() {
  const slug = $('kindSlug').value.trim();
  const body = { glyph: $('kindGlyph').value, label: $('kindLabel').value, enabled: $('kindEnabled').checked };
  const sort = $('kindSort').value.trim();
  if (sort !== '') body.sort = /^-?[0-9]+$/.test(sort) ? Number(sort) : sort;
  if (!(await putKind(slug, body))) return;
  resetKindForm();
  await afterKindChange();
}

async function toggleKind(slug) {
  const kind = kindOf(slug);
  if (!kind) return;
  if (!(await putKind(slug, { glyph: kind.glyph, label: kind.label, enabled: !kind.enabled }))) return;
  await afterKindChange();
}

// Timeline rows are `m-<id>`, thread panel rows `t-<id>`.
const rowId = (where, id) => `${where === 'thread' ? 't' : 'm'}-${id}`;

// The day separators, "새 메시지" mark and message rows of `messages`
// (oldest first) for the timeline or the thread panel.
function messageNodes(messages, room, where) {
  const bubble = S.style === 'bubble';
  const corrections = correctionsOnScreen();
  const items = messages.map((message) => ({ message, date: new Date(message.created_at) }));
  // The panel shows the root on top, apart from its replies, once loaded.
  const rootFirst = where === 'thread' && messages.length > 0 && messages[0].message_id === S.thread.root;
  const firstReply = (index) => rootFirst && index === 1;
  items.forEach((item, index) => {
    const previous = items[index - 1];
    item.day = !previous || dayKey(previous.date) !== dayKey(item.date);
    item.fresh = where === 'feed' && !S.q && S.newFrom === item.message.message_id;
    // In the bubble style the quote sits inside the bubble, so a reply does
    // not break a group. The thread's root stands apart from its replies.
    item.head = item.day || item.fresh || previous.message.sender_id !== item.message.sender_id
      || (Boolean(quoteOf(item.message, where)) && !bubble) || item.date - previous.date > GROUP_GAP_MS
      || firstReply(index);
  });
  items.forEach((item, index) => {
    item.tail = !items[index + 1] || items[index + 1].head;
  });
  const nodes = [];
  items.forEach((item, index) => {
    if (firstReply(index)) nodes.push(h('div', { class: 'day' }, h('span', { text: `답글 ${items.length - 1}개` })));
    else if (item.day) nodes.push(h('div', { class: 'day' }, h('span', { text: dayLabel(item.date) })));
    if (item.fresh) nodes.push(h('div', { class: 'newmark', text: '새 메시지' }));
    nodes.push(bubble ? bubbleMessage(item, room, where, corrections) : flatMessage(item, room, where, corrections));
  });
  return nodes;
}

function renderFeed() {
  const room = currentRoom();
  if (!room) {
    feed.replaceChildren();
    return;
  }
  if (S.feedError) {
    feed.replaceChildren(h('div', { class: 'empty', text: `불러오지 못했습니다 — ${failureText(S.feedError)}` }));
    return;
  }
  if (!S.feedLoaded) {
    feed.replaceChildren(h('div', { class: 'empty', text: '불러오는 중…' }));
    return;
  }
  if (S.msgs.length === 0) {
    feed.replaceChildren(h('div', { class: 'empty', text: S.q ? `“${S.q}”와 맞는 메시지가 없습니다` : '아직 메시지가 없습니다' }));
    return;
  }
  const nodes = [];
  if (S.hasOlder) nodes.push(h('div', { class: 'older', text: S.olderBusy ? '이전 메시지를 불러오는 중…' : '위로 스크롤하면 이전 메시지를 불러옵니다' }));
  nodes.push(...messageNodes(S.msgs, room, 'feed'));
  feed.replaceChildren(...nodes);
  unfoldShortTexts(feed);
}

// A folded text that fits in the clamp needs no "더 보기".
function unfoldShortTexts(list) {
  // A hidden list (the phone's room list) has no layout to measure.
  if (list.clientHeight === 0) return;
  for (const text of list.querySelectorAll('.text.clamp')) {
    if (text.scrollHeight - text.clientHeight > 1) continue;
    text.classList.remove('clamp');
    const more = text.nextElementSibling;
    if (more && more.classList.contains('more')) more.remove();
  }
}

const nearBottom = () => feed.scrollHeight - feed.scrollTop - feed.clientHeight < BOTTOM_SLACK_PX;

function toBottom() {
  feed.style.scrollBehavior = 'auto';
  feed.scrollTop = feed.scrollHeight;
  feed.style.scrollBehavior = '';
}

// Re-renders without moving what the reader is looking at.
function rerenderFeed() {
  const atBottom = nearBottom();
  const top = feed.scrollTop;
  renderFeed();
  feed.style.scrollBehavior = 'auto';
  if (atBottom) feed.scrollTop = feed.scrollHeight;
  else feed.scrollTop = top;
  feed.style.scrollBehavior = '';
}

function mergeMessages(items) {
  const known = new Set(S.msgs.map((message) => message.message_id));
  const fresh = items.filter((message) => !known.has(message.message_id));
  if (fresh.length === 0) return 0;
  S.msgs = S.msgs.concat(fresh).sort((a, b) => seqNum(a.event_seq) - seqNum(b.event_seq));
  for (const message of fresh) rememberSnippet(message);
  return fresh.length;
}

// Folds replies into their roots on the timeline: a reply newer than the
// root's summary raises its count (the summary the hub sent already holds
// the older ones), and a reply of the open thread joins the panel.
// Returns whether the timeline and the panel changed.
function foldReplies(replies) {
  let feedChanged = false;
  let threadChanged = false;
  for (const reply of replies) {
    if (seqNum(reply.event_seq) > seqNum(S.foldedSeq)) S.foldedSeq = reply.event_seq;
    rememberSnippet(reply);
    const root = findMessage(reply.thread_id);
    const summary = root && root.thread_summary;
    if (root && (!summary || seqNum(reply.event_seq) > seqNum(summary.last_event_seq))) {
      root.thread_summary = { reply_count: (summary ? summary.reply_count : 0) + 1, last_event_seq: reply.event_seq };
      feedChanged = true;
    }
    if (S.thread && S.thread.root === reply.thread_id && addThreadMessage(reply)) threadChanged = true;
  }
  return { feedChanged, threadChanged };
}

// New messages of the room, wherever they belong: top-level ones join the
// timeline, replies fold into their roots.
function absorb(messages) {
  const added = mergeMessages(messages.filter((message) => !message.thread_id));
  const folded = foldReplies(messages.filter((message) => message.thread_id));
  return { feedChanged: added > 0 || folded.feedChanged, threadChanged: folded.threadChanged };
}

// The timeline holds top-level messages only (S15).
function historyParams(extra) {
  return Object.assign({ room_id: S.room, top_level: 'true', q: S.q || null }, extra);
}

async function loadFeed() {
  const seq = S.feedSeq;
  // The cursor first, so no change between it and the page is lost (a
  // change seen twice changes nothing).
  await loadReactionCursor();
  if (seq !== S.feedSeq) return;
  try {
    const page = await request('GET', API.messages(), { params: historyParams({ limit: PAGE_SIZE }) });
    if (seq !== S.feedSeq) return;
    S.msgs = page.items.slice().reverse();
    S.hasOlder = page.has_more;
    S.olderCursor = page.next_before_event_seq || null;
    S.feedError = null;
  } catch (error) {
    if (seq !== S.feedSeq) return;
    S.feedError = asFailure(error);
  }
  S.feedLoaded = true;
}

async function loadOlder() {
  if (!S.hasOlder || S.olderBusy || !S.olderCursor) return false;
  S.olderBusy = true;
  const seq = S.feedSeq;
  try {
    const page = await request('GET', API.messages(), { params: historyParams({ before_event_seq: S.olderCursor, limit: PAGE_SIZE }) });
    if (seq !== S.feedSeq) return false;
    const fromBottom = feed.scrollHeight - feed.scrollTop;
    mergeMessages(page.items);
    S.hasOlder = page.has_more;
    S.olderCursor = page.next_before_event_seq || null;
    S.olderBusy = false;
    renderFeed();
    feed.style.scrollBehavior = 'auto';
    feed.scrollTop = feed.scrollHeight - fromBottom;
    feed.style.scrollBehavior = '';
    return true;
  } catch (_) {
    return false;
  } finally {
    S.olderBusy = false;
  }
}

// The poll's cursor: past the newest top-level message and every folded
// reply.
function newestSeq() {
  const last = S.msgs[S.msgs.length - 1];
  const top = last ? last.event_seq : '0';
  return seqNum(S.foldedSeq) > seqNum(top) ? S.foldedSeq : top;
}

async function pollFeed() {
  if (!S.token || !S.room || !S.feedLoaded || S.feedError || S.pollBusy || document.hidden) return;
  S.pollBusy = true;
  const seq = S.feedSeq;
  let feedChanged = false;
  let threadChanged = false;
  let arrived = 0;
  let reacted = null;
  try {
    let after = newestSeq();
    for (let page = 0; page < MAX_POLL_PAGES; page += 1) {
      // Every message after the cursor, replies included: they are folded.
      const result = await request('GET', API.messages(), { params: { room_id: S.room, q: S.q || null, after_event_seq: after, limit: POLL_PAGE_SIZE } });
      if (seq !== S.feedSeq) return;
      const changed = absorb(result.items);
      arrived += result.items.length;
      feedChanged = feedChanged || changed.feedChanged;
      threadChanged = threadChanged || changed.threadChanged;
      if (!result.has_more || !result.next_after_event_seq) break;
      after = result.next_after_event_seq;
    }
    reacted = await pollReactions(seq);
    if (!reacted) return;
  } catch (_) {
    // The next tick retries.
  } finally {
    S.pollBusy = false;
  }
  if (seq !== S.feedSeq) return;
  if (threadChanged) {
    rerenderThread();
    loadThreadView();
  }
  if (reacted) rerenderReactions({ feedChanged: reacted.feedChanged && !feedChanged, threadChanged: reacted.threadChanged && !threadChanged });
  if (arrived === 0) return;
  // Replies to roots off screen count as seen too: they are in this room.
  markSeen(currentRoom());
  if (feedChanged) rerenderFeed();
  renderRooms();
}

function flash(target) {
  if (!target) return;
  target.scrollIntoView({ block: 'center', behavior: 'smooth' });
  target.classList.remove('flash');
  void target.offsetWidth;
  target.classList.add('flash');
}

// The message `id` from what is loaded, or fetched once.
async function lookup(id) {
  const loaded = findMessage(id) || findThreadMessage(id);
  if (loaded) return loaded;
  const cached = S.quotes.get(id);
  if (cached && !cached.pending) return cached.missing ? null : cached;
  try {
    const message = await request('GET', API.message(id));
    S.quotes.set(id, message);
    return message;
  } catch (_) {
    return null;
  }
}

// Shows message `id`: in the open thread panel, in its thread (opened for
// it), or in the timeline, loading older pages as needed.
async function jumpTo(id) {
  if (S.thread && $(`t-${id}`)) {
    flash($(`t-${id}`));
    return;
  }
  const room = S.room;
  const message = await lookup(id);
  if (S.room !== room) return;
  if (message && message.thread_id) {
    await openThread(message.thread_id);
    flash($(`t-${id}`));
    return;
  }
  // An overlaid thread panel (narrow screens) would hide the timeline.
  if (S.thread && window.matchMedia('(max-width:1280px)').matches) closeThread();
  let target = $(`m-${id}`);
  for (let page = 0; !target && page < MAX_JUMP_PAGES; page += 1) {
    if (!(await loadOlder())) break;
    target = $(`m-${id}`);
  }
  flash(target);
}

// ---------------------------------------------------------------- swipe to reply

// Answering a timeline message opens its thread (S15); the panel answers
// the thread's last message unless one is quoted.
function startReply(id) {
  openThread(id);
}

let swipe = null;

function swipeOffset(dx) {
  const pull = Math.max(0, -dx);
  const travel = pull <= SWIPE_MAX_PX ? pull : SWIPE_MAX_PX + (pull - SWIPE_MAX_PX) * SWIPE_RESIST;
  return -travel;
}

function onSwipeDown(event) {
  if (event.pointerType !== 'touch' || !canWrite()) return;
  // A second finger (pinch) ends an undecided swipe.
  if (!event.isPrimary) {
    if (swipe && !swipe.claimed) swipe = null;
    return;
  }
  const row = event.target.closest('.msg, .b-row');
  if (!row || !(row.id.startsWith('m-') || row.id.startsWith('t-'))) return;
  swipe = {
    row, id: row.id.slice(2), inThread: row.id.startsWith('t-'), pointerId: event.pointerId,
    x: event.clientX, y: event.clientY, at: event.timeStamp, claimed: false, offset: 0, icon: null,
  };
}

function claimSwipe(state) {
  state.claimed = true;
  state.row.classList.remove('swipe-back');
  state.row.classList.add('swiping');
  state.icon = h('span', { class: 'swipe-ico', 'aria-hidden': 'true' }, icon('reply'));
  state.row.append(state.icon);
  try {
    state.row.setPointerCapture(state.pointerId);
  } catch (_) {
    // The pointer may already be gone; touch pointers are captured anyway.
  }
}

// One finger position, from either the pointer or the touch stream (whichever
// arrives first). Returns true while the browser must not scroll: the swipe is
// claimed, or the move so far leans left and is still too short to decide.
function swipeStep(x, y, timeStamp) {
  if (!swipe) return false;
  if (!swipe.row.isConnected) {
    swipe = null;
    return false;
  }
  const dx = x - swipe.x;
  const dy = y - swipe.y;
  if (!swipe.claimed) {
    // After a long press the browser keeps the gesture (text selection).
    if (timeStamp - swipe.at > SWIPE_LONG_PRESS_MS) {
      swipe = null;
      return false;
    }
    if (dx > 0 || Math.abs(dy) > Math.abs(dx)) {
      // Vertical or rightward: once decided the browser keeps it (scroll).
      if (Math.max(Math.abs(dx), Math.abs(dy)) >= SWIPE_CLAIM_PX) swipe = null;
      return false;
    }
    if (-dx < SWIPE_CLAIM_PX) return dx !== 0;
    claimSwipe(swipe);
  }
  // Once claimed the row follows dx only; vertical drift never cancels it.
  swipe.offset = swipeOffset(dx);
  swipe.row.style.transform = `translateX(${swipe.offset}px)`;
  // The icon stays put at the right edge while the row slides over it.
  swipe.icon.style.transform = `translateX(${-swipe.offset}px)`;
  swipe.icon.style.opacity = String(Math.min(1, -swipe.offset / SWIPE_REPLY_PX));
  return true;
}

function onSwipeMove(event) {
  if (event.pointerType !== 'touch' || !event.isPrimary) return;
  swipeStep(event.clientX, event.clientY, event.timeStamp);
}

// The only non-passive listener: cancelling the touchmove is what stops the
// browser from starting (or continuing) a vertical scroll under a swipe.
function onSwipeTouchMove(event) {
  if (!swipe || event.touches.length !== 1) return;
  const touch = event.touches[0];
  if (swipeStep(touch.clientX, touch.clientY, event.timeStamp) && event.cancelable) event.preventDefault();
}

function settleSwipe(state) {
  const { row } = state;
  row.classList.remove('swiping');
  row.classList.add('swipe-back');
  row.style.transform = '';
  if (state.icon) state.icon.remove();
  row.addEventListener('transitionend', () => row.classList.remove('swipe-back'), { once: true });
}

// The finger lifting off a swiped quote or "더 보기" must not also click it.
function swallowNextClick() {
  const swallow = (event) => {
    event.stopPropagation();
    event.preventDefault();
  };
  document.addEventListener('click', swallow, { capture: true, once: true });
  window.setTimeout(() => document.removeEventListener('click', swallow, { capture: true }), 400);
}

function onSwipeEnd(event) {
  if (!swipe || event.pointerType !== 'touch' || !event.isPrimary) return;
  const state = swipe;
  swipe = null;
  if (!state.claimed) {
    // A finger held still and lifted: the message's menu.
    if (event.type === 'pointerup' && event.timeStamp - state.at >= SWIPE_LONG_PRESS_MS) {
      openActions(state.row);
      swallowNextClick();
    }
    return;
  }
  settleSwipe(state);
  swallowNextClick();
  if (event.type !== 'pointerup' || -state.offset < SWIPE_REPLY_PX) return;
  if (navigator.vibrate) navigator.vibrate(SWIPE_HAPTIC_MS);
  if (state.inThread) quoteInThread(state.id);
  else startReply(state.id);
}

// The timeline and the thread panel answer a swipe the same way.
function bindSwipe() {
  feed.addEventListener('pointerdown', onSwipeDown, { passive: true });
  feed.addEventListener('pointermove', onSwipeMove, { passive: true });
  feed.addEventListener('touchmove', onSwipeTouchMove, { passive: false });
  feed.addEventListener('pointerup', onSwipeEnd, { passive: true });
  feed.addEventListener('pointercancel', onSwipeEnd, { passive: true });
  threadFeed.addEventListener('pointerdown', onSwipeDown, { passive: true });
  threadFeed.addEventListener('pointermove', onSwipeMove, { passive: true });
  threadFeed.addEventListener('touchmove', onSwipeTouchMove, { passive: false });
  threadFeed.addEventListener('pointerup', onSwipeEnd, { passive: true });
  threadFeed.addEventListener('pointercancel', onSwipeEnd, { passive: true });
  feed.addEventListener('contextmenu', onRowContextMenu);
  threadFeed.addEventListener('contextmenu', onRowContextMenu);
}

// ---------------------------------------------------------------- composer

// Writing into a room "나" is not a member of (an agent pair, a topic room
// without "나") is a barge-in: the room's members get it, "나" gets no reply.
function bargesIn(room) {
  const kind = roomKind(room);
  return kind === 'pair' || (kind === 'topic' && !room.members.includes(S.me));
}

function renderComposer() {
  const room = currentRoom();
  const writable = canWrite();
  const strips = [];
  if (!S.me) {
    strips.push(h('div', { class: 'strip note' }, h('span', { class: 'dot' }),
      h('span', { text: '보낼 명의가 없습니다. 설정 [owner] post_as가 비어 있어 읽기만 합니다.' })));
  } else if (isArchived(room)) {
    strips.push(h('div', { class: 'strip note' }, h('span', { class: 'dot' }),
      h('span', { text: '보관된 방은 읽기만 됩니다. 보내기·초대·내보내기가 막혀 있습니다.' })));
  } else if (room && bargesIn(room)) {
    strips.push(h('div', { class: 'strip' }, h('span', { class: 'dot' }),
      h('span', null, h('b', { text: S.me }), '으로 끼어들어 말합니다')));
  }
  if (writable && S.quiet) {
    strips.push(h('div', { class: 'strip' }, h('span', { class: 'dot' }),
      h('span', null, h('b', { text: '조용히' }), ' 보냅니다 — 아무도 깨우지 않고 inbox에만 남깁니다'),
      h('button', { class: 'x', data: { act: 'quiet' }, 'aria-label': '조용히 끄기' }, icon('close'))));
  }
  const fixing = writable && S.correcting && findMessage(S.correcting);
  if (fixing) strips.push(correctingStrip(fixing, 'uncorrect'));
  if (S.sendError) {
    strips.push(h('div', { class: 'strip bad', role: 'alert' }, h('span', { class: 'dot' }),
      h('span', null, h('b', { text: S.sendError.code }), ` — ${S.sendError.message}`),
      h('button', { class: 'x', data: { act: 'dismiss-send' }, 'aria-label': '오류 닫기' }, icon('close'))));
  }
  $('strips').replaceChildren(...strips);
  const input = $('input');
  input.disabled = !writable;
  if (!S.me) input.placeholder = '보낼 명의가 없습니다';
  else input.placeholder = writable ? '메시지 보내기' : '보관된 방은 읽기만 됩니다';
  $('quiet').disabled = !writable;
  $('quiet').setAttribute('aria-pressed', String(S.quiet));
  updateSendButton();
}

function updateSendButton() {
  const writable = canWrite();
  const ready = writable && !S.sending && Boolean($('input').value.trim());
  $('send').classList.toggle('ready', ready);
  $('send').disabled = !writable || S.sending;
}

function grow() {
  const box = $('input');
  box.style.height = 'auto';
  box.style.height = `${box.scrollHeight}px`;
  updateSendButton();
}

// "정정: <원문>" above a composer that is correcting `target`.
function correctingStrip(target, cancelAct) {
  return h('div', { class: 'strip' }, h('span', { class: 'dot' }),
    h('span', null, h('b', { text: '정정' }), ` — ${firstLine(target.body)}`),
    h('button', { class: 'x', data: { act: cancelAct }, 'aria-label': '정정 취소' }, icon('close')));
}

// The one destination of a correction (S15): a message outside threads is
// corrected in the room, a thread message in its thread (quoting it).
function correctionDestination(target, roomId) {
  return target.thread_id ? { reply_to: target.message_id } : { room_id: roomId };
}

async function send() {
  const room = currentRoom();
  const box = $('input');
  const text = box.value.trim();
  if (!text || !room || !canWrite() || S.sending) return;
  const quiet = S.quiet;
  const fixing = S.correcting && findMessage(S.correcting);
  const corrects = fixing ? fixing.message_id : null;
  // A retry of the same request keeps its key, so a lost response cannot
  // post the message twice (the hub compares room, body, reply, wake and
  // the corrected message).
  const pending = S.pendingSend;
  const same = pending && pending.room === room.room_id && pending.text === text
    && pending.corrects === corrects && pending.quiet === quiet;
  const clientKey = same ? pending.key : `console-${ulid()}`;
  S.pendingSend = { room: room.room_id, text, corrects, quiet, key: clientKey };
  S.sending = true;
  updateSendButton();
  // Exactly one destination: the room for a new message (answers go
  // through the thread panel).
  const body = Object.assign({ sender_id: S.me, body: text, client_key: clientKey },
    fixing ? correctionDestination(fixing, room.room_id) : { room_id: room.room_id });
  if (corrects) body.corrects = corrects;
  if (quiet) body.wake = false;
  try {
    const message = await request('POST', API.messages(), { json: body });
    S.pendingSend = null;
    S.sendError = null;
    S.correcting = null;
    S.newFrom = null;
    box.value = '';
    grow();
    if (S.q) {
      S.q = '';
      $('histQ').value = '';
      await reloadRoom();
    } else if (S.room === message.room_id) {
      const changed = absorb([message]);
      renderFeed();
      if (changed.threadChanged) rerenderThread();
    }
    renderComposer();
    toBottom();
    refreshRooms();
  } catch (error) {
    S.sendError = asFailure(error);
    renderComposer();
  } finally {
    S.sending = false;
    updateSendButton();
  }
}

// ------------------------------------------------------------ thread panel

// Opens the thread of `rootId` (a timeline message) in the panel: on wide
// screens on the right, on a phone over everything. Only one thread is
// open; its messages come from `thread_id=<root>` and the feed poll.
async function openThread(rootId) {
  if (S.thread && S.thread.root === rootId) {
    $('threadInput').focus();
    return;
  }
  S.thread = {
    root: rootId, room: S.room, msgs: [], loaded: false, error: null,
    view: null, hasOlder: false, olderCursor: null, olderBusy: false, replyTo: null,
    correcting: null, sending: false, sendError: null, pendingSend: null, muteBusy: false,
    muteError: null,
  };
  $('threadInput').value = '';
  // Narrow screens show one overlay at a time.
  if (window.matchMedia('(max-width:1280px)').matches) app.classList.add('info-off');
  renderThread();
  await loadThread();
  if (canWrite() && !window.matchMedia('(max-width:720px)').matches) $('threadInput').focus();
}

function closeThread() {
  S.thread = null;
  renderThread();
  renderTop();
}

const threadIsCurrent = (thread) => S.thread === thread;

async function loadThread() {
  const thread = S.thread;
  if (!thread) return;
  // A timeline root without a summary has no reply yet: the hub would
  // answer thread_not_found, so the panel shows the root alone.
  const known = findMessage(thread.root);
  if (known && !known.thread_summary) {
    thread.msgs = [known];
    thread.loaded = true;
    renderThread();
    return;
  }
  const views = loadThreadView();
  try {
    const page = await request('GET', API.messages(), { params: { room_id: thread.room, thread_id: thread.root, limit: THREAD_PAGE_SIZE } });
    if (!threadIsCurrent(thread)) return;
    thread.msgs = page.items.slice().reverse();
    thread.hasOlder = page.has_more;
    thread.olderCursor = page.next_before_event_seq || null;
  } catch (error) {
    if (!threadIsCurrent(thread)) return;
    const failure = asFailure(error);
    if (failure.code !== 'thread_not_found') {
      thread.error = failure;
    } else {
      // No reply yet: the thread is the root alone until the first answer.
      const root = await lookup(thread.root);
      if (!threadIsCurrent(thread)) return;
      thread.msgs = root ? [root] : [];
    }
  }
  await views;
  if (!threadIsCurrent(thread)) return;
  thread.loaded = true;
  renderThread();
  threadToBottom();
}

// The participants and counts; a thread without replies has none yet.
async function loadThreadView() {
  const thread = S.thread;
  if (!thread) return;
  try {
    const view = await request('GET', API.thread(thread.root));
    if (threadIsCurrent(thread)) thread.view = view;
  } catch (error) {
    if (threadIsCurrent(thread) && asFailure(error).code === 'thread_not_found') thread.view = null;
  }
  if (threadIsCurrent(thread) && thread.loaded) renderThreadMeta();
}

async function loadOlderInThread() {
  const thread = S.thread;
  if (!thread || !thread.hasOlder || thread.olderBusy || !thread.olderCursor) return;
  thread.olderBusy = true;
  renderThread();
  try {
    const page = await request('GET', API.messages(), { params: { room_id: thread.room, thread_id: thread.root, before_event_seq: thread.olderCursor, limit: THREAD_PAGE_SIZE } });
    if (!threadIsCurrent(thread)) return;
    for (const message of page.items) addThreadMessage(message);
    thread.hasOlder = page.has_more;
    thread.olderCursor = page.next_before_event_seq || null;
  } catch (_) {
    // The button stays; another press retries.
  } finally {
    thread.olderBusy = false;
  }
  if (threadIsCurrent(thread)) rerenderThread();
}

// Adds a message of the open thread in event_seq order; false if known.
function addThreadMessage(message) {
  const thread = S.thread;
  if (!thread || thread.msgs.some((known) => known.message_id === message.message_id)) return false;
  thread.msgs = thread.msgs.concat([message]).sort((a, b) => seqNum(a.event_seq) - seqNum(b.event_seq));
  return true;
}

// What the panel's composer answers: the quoted message, or else the
// thread's last message (the root while it has no reply).
function threadReplyTarget(thread) {
  if (thread.replyTo) return thread.replyTo;
  const last = thread.msgs[thread.msgs.length - 1];
  return last ? last.message_id : thread.root;
}

function quoteInThread(id) {
  if (!S.thread || !canWrite()) return;
  S.thread.replyTo = id;
  S.thread.correcting = null;
  renderThreadComposer();
  $('threadInput').focus();
}

function correctInThread(id) {
  if (!S.thread || !canWrite()) return;
  S.thread.correcting = id;
  S.thread.replyTo = null;
  renderThreadComposer();
  $('threadInput').focus();
}

// "나" among the participants, or null: only a participant may mute.
function threadMe(thread) {
  if (!thread.view || !S.me) return null;
  return thread.view.participants.find((participant) => participant.agent_id === S.me) || null;
}

async function toggleThreadMute() {
  const thread = S.thread;
  const me = thread && threadMe(thread);
  if (!me || thread.muteBusy) return;
  thread.muteBusy = true;
  thread.muteError = null;
  renderThreadMeta();
  try {
    // `wake` is the opposite of the new mute; the owner names "나".
    const changed = await request('PUT', API.participation(thread.root), { json: { wake: me.muted, agent_id: S.me } });
    if (threadIsCurrent(thread)) me.muted = changed.muted;
  } catch (error) {
    if (threadIsCurrent(thread)) thread.muteError = asFailure(error);
  } finally {
    thread.muteBusy = false;
  }
  if (threadIsCurrent(thread)) renderThreadMeta();
}

function renderThread() {
  const thread = S.thread;
  $('thread').hidden = !thread;
  app.classList.toggle('thread-on', Boolean(thread));
  if (!thread) {
    threadFeed.replaceChildren();
    $('threadMeta').replaceChildren();
    $('threadStrips').replaceChildren();
    return;
  }
  const room = allRooms().find((candidate) => candidate.room_id === thread.room);
  $('threadTitle').replaceChildren(h('b', { text: '스레드' }), room ? h('span', { class: 'sub', text: roomTitle(room) }) : null);
  renderThreadMeta();
  renderThreadFeed();
  renderThreadComposer();
}

// Re-renders the panel's list without moving what the reader looks at.
function rerenderThread() {
  if (!S.thread) return;
  const atBottom = threadFeed.scrollHeight - threadFeed.scrollTop - threadFeed.clientHeight < BOTTOM_SLACK_PX;
  const top = threadFeed.scrollTop;
  renderThreadFeed();
  renderThreadComposer();
  threadFeed.scrollTop = atBottom ? threadFeed.scrollHeight : top;
}

function threadToBottom() {
  threadFeed.scrollTop = threadFeed.scrollHeight;
}

function renderThreadMeta() {
  const thread = S.thread;
  if (!thread) return;
  const nodes = [];
  const root = thread.msgs.find((message) => message.message_id === thread.root) || findMessage(thread.root);
  const claim = root ? claimBadge(root) : null;
  if (claim) nodes.push(h('div', { class: 'claim-row' }, claim));
  const participants = thread.view ? thread.view.participants : [];
  if (participants.length > 0) {
    nodes.push(h('div', { class: 'chips', 'aria-label': '참여자' },
      h('span', { class: 'muted', text: `참여자 ${participants.length}` }),
      participants.map((participant) => h('span', {
        class: `chip${participant.muted ? ' muted-chip' : ''}`,
        title: `${JOINED_LABEL[participant.joined_via] || participant.joined_via}${participant.muted ? ' · 알림 끔' : ''}`,
      }, av(participant.agent_id, 16), nameOf(participant.agent_id)))));
  } else if (thread.loaded) {
    nodes.push(h('p', { class: 'muted flush', text: '아직 답글이 없습니다. 첫 답글이 스레드를 엽니다.' }));
  }
  const me = threadMe(thread);
  if (me) {
    nodes.push(h('div', { class: 'toggle-row' },
      h('span', null, '이 스레드 알림', h('small', { text: me.muted ? '꺼짐 — 지목될 때만 깨어납니다' : '켜짐 — 스레드 글로 깨어납니다' })),
      h('button', {
        class: 'switch', role: 'switch', 'aria-checked': String(!me.muted), 'aria-label': '이 스레드 알림',
        data: { act: 'thread-mute' }, disabled: thread.muteBusy,
      })));
  }
  if (thread.muteError) nodes.push(h('div', { class: 'err', role: 'alert', text: failureText(thread.muteError) }));
  $('threadMeta').replaceChildren(...nodes);
}

function renderThreadFeed() {
  const thread = S.thread;
  const room = allRooms().find((candidate) => candidate.room_id === thread.room);
  if (thread.error) {
    threadFeed.replaceChildren(h('div', { class: 'empty', text: `불러오지 못했습니다 — ${failureText(thread.error)}` }));
    return;
  }
  if (!thread.loaded || !room) {
    threadFeed.replaceChildren(h('div', { class: 'empty', text: '불러오는 중…' }));
    return;
  }
  const nodes = [];
  if (thread.hasOlder) {
    nodes.push(h('button', { class: 'older-btn', data: { act: 'thread-older' }, disabled: thread.olderBusy,
      text: thread.olderBusy ? '이전 답글을 불러오는 중…' : '이전 답글 불러오기' }));
  }
  nodes.push(...messageNodes(thread.msgs, room, 'thread'));
  threadFeed.replaceChildren(...nodes);
  unfoldShortTexts(threadFeed);
}

function renderThreadComposer() {
  const thread = S.thread;
  if (!thread) return;
  const writable = canWrite();
  const strips = [];
  const fixing = writable && thread.correcting && findThreadMessage(thread.correcting);
  if (fixing) {
    strips.push(correctingStrip(fixing, 'thread-uncorrect'));
  } else if (writable && thread.replyTo) {
    const target = findThreadMessage(thread.replyTo);
    if (target) {
      strips.push(h('div', { class: 'strip' },
        h('span', null, h('b', { text: nameOf(target.sender_id) }), '에게 답장'),
        h('button', { class: 'x', data: { act: 'thread-unquote' }, 'aria-label': '답장 취소' }, icon('close'))));
    }
  }
  if (thread.sendError) {
    strips.push(h('div', { class: 'strip bad', role: 'alert' }, h('span', { class: 'dot' }),
      h('span', null, h('b', { text: thread.sendError.code }), ` — ${thread.sendError.message}`),
      h('button', { class: 'x', data: { act: 'thread-dismiss' }, 'aria-label': '오류 닫기' }, icon('close'))));
  }
  $('threadStrips').replaceChildren(...strips);
  const input = $('threadInput');
  input.disabled = !writable;
  input.placeholder = writable ? '스레드에 답장' : (S.me ? '보관된 방은 읽기만 됩니다' : '보낼 명의가 없습니다');
  updateThreadSendButton();
}

function updateThreadSendButton() {
  const thread = S.thread;
  const writable = canWrite() && Boolean(thread);
  const busy = Boolean(thread && thread.sending);
  $('threadSend').classList.toggle('ready', writable && !busy && Boolean($('threadInput').value.trim()));
  $('threadSend').disabled = !writable || busy;
}

function growThread() {
  const box = $('threadInput');
  box.style.height = 'auto';
  box.style.height = `${box.scrollHeight}px`;
  updateThreadSendButton();
}

// Sends into the open thread: `reply_to` only (the hub keeps it in the
// thread), or a correction by the same destination rule as the room's.
async function sendThread() {
  const thread = S.thread;
  const box = $('threadInput');
  const text = box.value.trim();
  if (!thread || !text || !canWrite() || thread.sending) return;
  const fixing = thread.correcting && findThreadMessage(thread.correcting);
  const replyTo = fixing ? null : threadReplyTarget(thread);
  const corrects = fixing ? fixing.message_id : null;
  const pending = thread.pendingSend;
  const same = pending && pending.text === text && pending.replyTo === replyTo && pending.corrects === corrects;
  const clientKey = same ? pending.key : `console-${ulid()}`;
  thread.pendingSend = { text, replyTo, corrects, key: clientKey };
  thread.sending = true;
  updateThreadSendButton();
  const body = Object.assign({ sender_id: S.me, body: text, client_key: clientKey },
    fixing ? correctionDestination(fixing, thread.room) : { reply_to: replyTo });
  if (corrects) body.corrects = corrects;
  try {
    const message = await request('POST', API.messages(), { json: body });
    if (!threadIsCurrent(thread)) return;
    thread.pendingSend = null;
    thread.sendError = null;
    thread.replyTo = null;
    thread.correcting = null;
    box.value = '';
    growThread();
    const changed = S.room === message.room_id ? absorb([message]) : { feedChanged: false };
    if (changed.feedChanged) rerenderFeed();
    rerenderThread();
    threadToBottom();
    loadThreadView();
    refreshRooms();
  } catch (error) {
    if (threadIsCurrent(thread)) thread.sendError = asFailure(error);
    renderThreadComposer();
  } finally {
    thread.sending = false;
    updateThreadSendButton();
  }
}

// -------------------------------------------------------------------- info

function infoAgentId(room) {
  const kind = roomKind(room);
  if (kind === 'all' || kind === 'topic') return S.member;
  if (kind === 'dm') return peerOf(room);
  return S.tab[room.room_id] || room.members[0];
}

function renderInfo() {
  const room = currentRoom();
  const nodes = [];
  if (!room) {
    $('infoBody').replaceChildren();
    return;
  }
  const kind = roomKind(room);
  if (kind === 'all' && !S.member) {
    const block = h('div', { class: 'blk' }, h('h3', { text: `멤버 ${room.members.length}` }));
    for (const id of room.members) {
      const agent = agentOf(id);
      block.append(h('button', { class: 'member', data: { act: 'member', id } }, avDot(id, 32),
        h('span', { class: 'who' }, nameOf(id), h('small', { text: (agent && agent.profile.role) || id }), presenceLine(agent)), pill(agent)));
    }
    if (room.members.length === 0) block.append(h('p', { class: 'muted flush', text: '가입한 에이전트가 없습니다' }));
    nodes.push(block);
  } else if ((kind === 'all' || kind === 'topic') && S.member) {
    nodes.push(h('button', { class: 'linkback', data: { act: 'member', id: '' } }, icon('back'), '멤버'));
    nodes.push(...agentBlock(S.member));
  } else if (kind === 'topic') {
    nodes.push(...topicBlocks(room));
  } else if (kind === 'dm') {
    nodes.push(...agentBlock(peerOf(room)));
  } else {
    const current = infoAgentId(room);
    nodes.push(h('div', { class: 'seg', role: 'tablist' }, room.members.map((id) => h('button', {
      role: 'tab', 'aria-selected': String(id === current), data: { act: 'tab', id },
    }, av(id, 16), nameOf(id)))));
    nodes.push(...agentBlock(current));
  }
  $('infoBody').replaceChildren(...nodes);
}

// A topic room's panel: what it is, its members (each removable), an
// invite picked from the directory, and archiving. An archived room keeps
// the list but disables every control.
function topicBlocks(room) {
  const archived = isArchived(room);
  const locked = archived || S.roomBusy;
  const about = h('div', { class: 'blk' },
    h('div', { class: 'prof' }, h('span', { class: 'hashbox' }, icon('topic')),
      h('div', null, h('div', { class: 'nm', text: roomTitle(room) }), h('div', { class: 'id', text: room.room_id }))),
    h('dl', { class: 'kv' },
      h('dt', { text: '만든 때' }), h('dd', { text: stamp(room.created_at) }),
      h('dt', { text: '상태' }), h('dd', { text: archived ? `보관됨 · ${stamp(room.archived_at)}` : '열림' })),
    archived ? h('p', { class: 'warnbox', text: '보관된 방은 읽기만 됩니다. 보내기·초대·내보내기·보관 해제는 할 수 없습니다.' }) : null);
  const members = h('div', { class: 'blk' }, h('h3', { text: `멤버 ${room.members.length}` }));
  for (const id of room.members) {
    const agent = agentOf(id);
    const detail = id === S.me ? `나 · ${id}` : (agent && agent.profile.role) || id;
    members.append(h('div', { class: 'member-row' },
      h('button', { class: 'member', data: { act: 'member', id } }, avDot(id, 32),
        h('span', { class: 'who' }, nameOf(id), h('small', { text: detail }), presenceLine(agent))),
      h('button', { class: 'btn sm ghost-bad', data: { act: 'kick', id }, disabled: locked, text: '내보내기' })));
  }
  if (room.members.length === 0) members.append(h('p', { class: 'muted flush', text: '아직 member가 없습니다. 아래에서 초대합니다.' }));
  const candidates = Array.from(S.agents.values()).filter((agent) => !room.members.includes(agent.agent_id));
  members.append(h('div', { class: 'invite' },
    h('select', { id: 'inviteSel', 'aria-label': '초대할 에이전트', disabled: locked || candidates.length === 0 },
      candidates.length === 0
        ? h('option', { value: '', text: '초대할 에이전트가 없습니다' })
        : candidates.map((agent) => h('option', { value: agent.agent_id, text: `${agent.name || agent.agent_id} (${agent.agent_id})` }))),
    h('button', { class: 'btn sm', data: { act: 'invite' }, disabled: locked || candidates.length === 0, text: '초대' })));
  const close = h('div', { class: 'blk' }, h('div', { class: 'toggle-row' },
    h('span', null, '보관', h('small', { text: archived ? '보관한 방입니다. 되돌릴 수 없습니다.' : '읽기만 되는 방으로 닫습니다. 되돌릴 수 없습니다.' })),
    h('button', { class: 'btn sm ghost-bad', data: { act: 'archive' }, disabled: locked, text: archived ? '보관됨' : '보관' })));
  const nodes = [about, members, close];
  if (S.roomError) nodes.push(h('div', { class: 'blk' }, h('div', { class: 'err', role: 'alert', text: failureText(S.roomError) })));
  return nodes;
}

// Poll-driven renders skip the panel while the user is typing into it.
function renderInfoPassively() {
  const active = document.activeElement;
  if (S.edit || S.receiverEdit || (active && $('info').contains(active) && active.matches('input, textarea, select'))) return;
  renderInfo();
}

function agentBlock(id) {
  const agent = agentOf(id);
  if (!agent) {
    return [h('div', { class: 'blk' }, h('div', { class: 'prof' }, av(id, 56), h('div', null, h('div', { class: 'nm', text: id }), h('div', { class: 'id', text: id }))),
      h('p', { class: 'muted flush' }, h('b', { text: '등록되지 않은 agent' }), ' — 삭제되었거나 등록된 적 없는 ID입니다. 남은 대화는 그대로 읽을 수 있습니다.'))];
  }
  const nodes = [profileBlock(agent)];
  nodes.push(h('div', { class: 'seg sub', role: 'tablist' },
    h('button', { role: 'tab', 'aria-selected': String(S.infoTab === 'settings'), data: { act: 'info-tab', tab: 'settings' }, text: '전달·설정' }),
    h('button', { role: 'tab', 'aria-selected': String(S.infoTab === 'inbox'), data: { act: 'info-tab', tab: 'inbox' }, text: 'inbox' })));
  if (S.infoTab === 'inbox') {
    nodes.push(inboxBlock(agent));
    return nodes;
  }
  const status = deliveryOf(id);
  if (id === S.me && agent.receiver_url === null) {
    nodes.push(h('div', { class: 'blk' }, h('h3', { text: '전달' }),
      h('p', { class: 'muted flush', text: '소유자는 receiver 알림 대상이 아닙니다. 브라우저와 CLI로 직접 읽습니다.' })));
  } else {
    nodes.push(deliveryBlock(agent, status));
  }
  nodes.push(receiverBlock(agent), joinBlock(agent));
  if (S.actionError && S.actionError.id === id) {
    nodes.push(h('div', { class: 'blk' }, h('div', { class: 'err', role: 'alert', text: failureText(S.actionError.failure) })));
  }
  return nodes;
}

function profileBlock(agent) {
  const id = agent.agent_id;
  const profile = agent.profile;
  const field = (key, label, control, shown) => [h('dt', { text: label }), h('dd', null, S.edit ? control : shown)];
  const select = (key, labels, value) => h('select', { id: `f-${key}` },
    h('option', { value: '', text: '선언 안 함', selected: value === null }),
    Object.entries(labels).map(([option, text]) => h('option', { value: option, text, selected: value === option })));
  const rows = [h('dt', { text: '상태' }), h('dd', null, presenceLine(agent))];
  if (S.edit) rows.push(field('name', '이름', h('input', { id: 'f-name', value: agent.name || '' }), null));
  rows.push(field('gender', '성별형', select('gender_form', GENDER_LABEL, profile.gender_form), profile.gender_form ? GENDER_LABEL[profile.gender_form] : NONE_TEXT));
  rows.push(field('age', '나이대', select('age_band', AGE_LABEL, profile.age_band), profile.age_band ? AGE_LABEL[profile.age_band] : NONE_TEXT));
  rows.push(field('role', '역할', h('input', { id: 'f-role', value: profile.role || '' }), profile.role || NONE_TEXT));
  const intro = h('textarea', { id: 'f-intro' });
  intro.value = profile.intro || '';
  rows.push(field('intro', '소개', intro, profile.intro || NONE_TEXT));
  return h('div', { class: 'blk' },
    h('div', { class: 'prof' }, avDot(id, 56), h('div', null, h('div', { class: 'nm', text: agent.name || id }), h('div', { class: 'id', text: id }))),
    S.edit ? h('div', { class: 'row-start' },
      h('button', { class: 'btn sm', data: { act: 'avatar-upload', id }, text: '사진 바꾸기' }),
      h('button', { class: 'btn sm', data: { act: 'avatar-clear', id }, disabled: !agent.avatar, text: '사진 지우기' })) : null,
    h('dl', { class: 'kv' }, rows),
    h('div', { class: 'row-end' }, S.edit
      ? [h('button', { class: 'btn sm', data: { act: 'edit-cancel' }, text: '취소' }), h('button', { class: 'btn sm primary', data: { act: 'edit-save', id }, text: '저장' })]
      : h('button', { class: 'btn sm', data: { act: 'edit' }, text: '편집' })),
    S.profileError ? h('div', { class: 'err', role: 'alert', text: S.profileError }) : null);
}

function deliveryOf(id) {
  const entry = S.delivery.get(id);
  if (!entry || Date.now() - entry.at > DELIVERY_STALE_MS) refreshDelivery(id);
  return entry ? entry.status : null;
}

async function refreshDelivery(id) {
  const entry = S.delivery.get(id);
  if (entry && entry.busy) return;
  S.delivery.set(id, Object.assign({ status: null, at: 0 }, entry, { busy: true, at: Date.now() }));
  try {
    const status = await request('GET', API.delivery(id));
    S.delivery.set(id, { status, at: Date.now(), busy: false });
  } catch (_) {
    S.delivery.set(id, Object.assign({}, S.delivery.get(id), { busy: false }));
    return;
  }
  if (infoAgentIdNow() === id) renderInfoPassively();
}

function infoAgentIdNow() {
  const room = currentRoom();
  return room ? infoAgentId(room) : null;
}

function deliveryBlock(agent, status) {
  const read = status ? seqNum(status.read_marker) : null;
  const head = status ? seqNum(status.inbox_head_seq) : null;
  const percent = status && head > 0 ? Math.round((read / head) * 100) : 100;
  const state = (status && status.state) || agent.delivery.state;
  const last = status && status.last_attempt_at
    ? `${relative(status.last_attempt_at)}${status.last_error ? ` · ${status.last_error}` : ''}`
    : NONE_TEXT;
  return h('div', { class: 'blk' },
    h('h3', null, '전달 상태 ', pill(agent, status)),
    h('div', { class: 'stat' },
      h('span', { text: '읽음 / head' }), h('span', { class: 'num', text: status ? `${read} / ${head}` : NONE_TEXT }),
      h('span', { text: '미확인' }), h('span', { class: 'num', text: status ? String(head - read) : NONE_TEXT }),
      h('span', { text: '마지막 시도' }), h('span', { text: last })),
    h('div', { class: 'bar' }, h('i', { width: `${percent}%` })),
    state === 'blocked' ? h('div', { class: 'err', text: `전달이 멈췄습니다${status && status.last_error ? ` (${status.last_error})` : ''}. receiver를 고치거나 retry로 다시 시도합니다.` }) : null,
    h('div', { class: 'row-end' }, h('button', { class: 'btn sm', data: { act: 'retry', id: agent.agent_id }, disabled: state === 'idle' || state === 'no_receiver', text: 'retry' })));
}

function receiverBlock(agent) {
  const id = agent.agent_id;
  if (S.receiverEdit === id) {
    return h('div', { class: 'blk' }, h('h3', { text: 'receiver' }),
      h('div', { class: 'url' }, h('input', { id: 'recvInput', placeholder: 'receiver URL (http·https)', spellcheck: 'false', autocomplete: 'off' })),
      h('div', { class: 'row-end' },
        h('button', { class: 'btn sm', data: { act: 'receiver-cancel' }, text: '취소' }),
        h('button', { class: 'btn sm primary', data: { act: 'receiver-save', id }, text: '저장' })));
  }
  return h('div', { class: 'blk' }, h('h3', { text: 'receiver' }),
    h('div', { class: 'url' }, h('span', { text: agent.receiver_url ? maskedUrl(agent.receiver_url) : '연결 안 됨' })),
    h('div', { class: 'row-end' },
      agent.receiver_url ? h('button', { class: 'btn sm', data: { act: 'receiver-clear', id }, text: '끊기' }) : null,
      h('button', { class: 'btn sm', data: { act: 'receiver-edit', id }, text: agent.receiver_url ? '바꾸기' : '설정' })));
}

function joinBlock(agent) {
  return h('div', { class: 'blk' }, h('div', { class: 'toggle-row' },
    h('span', null, 'all 가입', h('small', { text: agent.joined_all ? '# all 메시지를 받습니다' : '# all 메시지를 받지 않습니다' })),
    h('button', { class: 'switch', role: 'switch', 'aria-checked': String(agent.joined_all), 'aria-label': 'all 가입', data: { act: 'join', id: agent.agent_id } })));
}

function inboxBlock(agent) {
  const id = agent.agent_id;
  const state = S.inbox.get(id);
  if (!state) loadInbox(id, false);
  const block = h('div', { class: 'blk' }, h('h3', null, 'inbox',
    state && state.readMarker !== null ? h('span', { class: 'num', text: `읽음 ${state.readMarker} · head ${state.head}` }) : null));
  block.append(h('p', { class: 'muted flush', text: '읽음 위치 뒤의 항목입니다. 콘솔은 읽음 위치를 바꾸지 않습니다.' }));
  if (!state || (state.busy && state.items.length === 0)) {
    block.append(h('p', { class: 'muted', text: '불러오는 중…' }));
    return block;
  }
  if (state.error) block.append(h('div', { class: 'err', text: failureText(state.error) }));
  const list = h('div');
  for (const item of state.items) {
    // An event kind this page does not know yet is listed, never parsed.
    if (item.kind !== 'message') {
      list.append(h('div', { class: 'ibx' }, h('span', { class: 'num', text: `#${item.inbox_seq}` }),
        h('div', { class: 'hd' }, h('b', { text: '알 수 없는 사건' }), h('span', { text: String(item.kind) }))));
      continue;
    }
    const summary = allRooms().find((room) => room.room_id === item.room_id);
    list.append(h('div', { class: 'ibx' },
      h('span', { class: 'num', text: `#${item.inbox_seq}` }),
      h('div', { class: 'hd' }, h('b', { text: nameOf(item.sender_id) }), h('span', { text: `${summary ? roomTitle(summary) : item.room_id} · ${relative(item.created_at)}` })),
      h('div', { class: 'bd', text: firstLine(item.body) })));
  }
  if (state.items.length === 0 && !state.error) list.append(h('p', { class: 'muted', text: '미확인 항목이 없습니다' }));
  block.append(list, h('div', { class: 'row-end' },
    h('button', { class: 'btn sm', data: { act: 'inbox-refresh', id }, text: '새로고침' }),
    state.hasMore ? h('button', { class: 'btn sm', data: { act: 'inbox-more', id }, disabled: state.busy, text: '더 보기' }) : null));
  return block;
}

async function loadInbox(id, more) {
  const previous = S.inbox.get(id);
  if (previous && previous.busy) return;
  const state = more && previous
    ? Object.assign({}, previous, { busy: true })
    : { items: [], readMarker: null, head: null, until: null, nextAfter: null, hasMore: false, error: null, busy: true };
  S.inbox.set(id, state);
  const params = more ? { after_seq: state.nextAfter, until_seq: state.until, limit: INBOX_PAGE_SIZE } : { limit: INBOX_PAGE_SIZE };
  try {
    const page = await request('GET', API.inbox(id), { params });
    state.items = state.items.concat(page.items);
    state.readMarker = page.read_marker;
    state.head = page.snapshot_head_seq;
    state.until = page.snapshot_head_seq;
    state.nextAfter = page.next_after_seq;
    state.hasMore = page.has_more;
    state.error = null;
  } catch (error) {
    state.error = asFailure(error);
  }
  state.busy = false;
  if (infoAgentIdNow() === id) renderInfo();
}

// ------------------------------------------------------------- owner writes

async function saveProfile(id) {
  const agent = agentOf(id);
  if (!agent) return;
  const value = (key) => $(`f-${key}`).value.trim();
  const orNull = (text) => (text === '' ? null : text);
  const next = {
    name: value('name'),
    gender_form: orNull(value('gender_form')),
    age_band: orNull(value('age_band')),
    role: orNull(value('role')),
    intro: orNull($('f-intro').value.trim()),
  };
  const body = {};
  if (next.name !== '' && next.name !== agent.name) body.name = next.name;
  for (const key of ['gender_form', 'age_band', 'role', 'intro']) {
    if (next[key] !== agent.profile[key]) body[key] = next[key];
  }
  if (Object.keys(body).length === 0) {
    S.edit = false;
    S.profileError = null;
    renderInfo();
    return;
  }
  try {
    const saved = await request('PUT', API.profile(id), { json: body });
    S.agents.set(id, Object.assign({}, agent, { name: saved.name, profile: saved.profile }));
    S.edit = false;
    S.profileError = null;
    renderAll();
  } catch (error) {
    S.profileError = failureText(asFailure(error));
    renderInfo();
  }
}

async function uploadAvatar(id, file) {
  S.profileError = null;
  if (file.size > AVATAR_MAX_BYTES) {
    S.profileError = `avatar_too_large — 사진은 ${AVATAR_MAX_BYTES / 1024} KiB 이하만 올릴 수 있습니다`;
    renderInfo();
    return;
  }
  try {
    const stored = await request('PUT', API.avatar(id), { blob: file, contentType: file.type || 'application/octet-stream' });
    updateAgent(id, { avatar: { sha256: stored.avatar.sha256, updated_at: stored.avatar.updated_at } });
    ensureAvatars();
  } catch (error) {
    S.profileError = failureText(asFailure(error));
  }
  renderInfo();
}

async function clearAvatar(id) {
  S.profileError = null;
  try {
    await request('DELETE', API.avatar(id));
    updateAgent(id, { avatar: null });
    forgetAvatar(id);
    repaintAvatars(id);
  } catch (error) {
    S.profileError = failureText(asFailure(error));
  }
  renderInfo();
}

function updateAgent(id, changes) {
  const agent = agentOf(id);
  if (agent) S.agents.set(id, Object.assign({}, agent, changes));
}

// Runs an owner write from the info panel and shows its error there.
async function ownerAction(id, work) {
  S.actionError = null;
  try {
    await work();
  } catch (error) {
    S.actionError = { id, failure: asFailure(error) };
  }
  S.delivery.delete(id);
  await refreshDirectory();
  renderInfo();
}

function saveReceiver(id) {
  const url = $('recvInput').value.trim();
  if (!url) return;
  S.receiverEdit = null;
  ownerAction(id, () => request('PATCH', API.receiver(id), { json: { receiver_url: url } }));
}

function clearReceiver(id) {
  if (!window.confirm(`${nameOf(id)}의 receiver를 끊을까요? 알림이 멈추고 URL은 다시 입력해야 합니다.`)) return;
  ownerAction(id, () => request('PATCH', API.receiver(id), { json: { receiver_url: null } }));
}

function toggleJoin(id) {
  const agent = agentOf(id);
  if (!agent) return;
  ownerAction(id, async () => {
    await request('PUT', API.membership(id), { json: { joined: !agent.joined_all } });
    await refreshRooms();
  });
}

function retryDelivery(id) {
  ownerAction(id, () => request('POST', API.retry(id)));
}

// -------------------------------------------------------------- topic rooms

// A confirmation in the page's own dialog; resolves true only for the
// confirm button (Escape, cancel and logout resolve false).
function ask(title, text, go) {
  const dialog = $('ask');
  $('askTitle').textContent = title;
  $('askText').textContent = text;
  $('askGo').textContent = go;
  dialog.returnValue = '';
  dialog.showModal();
  return new Promise((resolve) => {
    dialog.addEventListener('close', () => resolve(dialog.returnValue === 'ok'), { once: true });
  });
}

// Runs an owner write on the open topic room and shows its error in the panel.
async function roomAction(work) {
  S.roomError = null;
  S.roomBusy = true;
  renderInfo();
  try {
    await work();
  } catch (error) {
    S.roomError = asFailure(error);
  }
  S.roomBusy = false;
  await refreshRooms();
  renderInfo();
}

function inviteMember(room) {
  const id = $('inviteSel').value;
  if (!id || isArchived(room)) return;
  roomAction(() => request('PUT', API.roomMember(room.room_id, id)));
}

async function removeMember(room, id) {
  if (isArchived(room)) return;
  const confirmed = await ask('멤버 내보내기',
    `${nameOf(id)}(${id})를 ${roomTitle(room)}에서 내보낼까요? 이 방을 더는 읽거나 쓰지 못하고 새 글도 받지 않습니다. 이미 받은 inbox 항목은 남습니다. 다시 초대할 수 있습니다.`,
    '내보내기');
  if (!confirmed || S.room !== room.room_id) return;
  roomAction(() => request('DELETE', API.roomMember(room.room_id, id)));
}

async function archiveRoom(room) {
  if (isArchived(room)) return;
  const confirmed = await ask('주제 방 보관',
    `${roomTitle(room)}(${room.room_id})를 보관할까요? 보관한 방은 읽기만 되고 보내기·초대·내보내기가 막힙니다. 되돌릴 수 없습니다.`,
    '보관');
  if (!confirmed || S.room !== room.room_id) return;
  roomAction(async () => {
    await request('POST', API.roomArchive(room.room_id));
    S.correcting = null;
    S.quiet = false;
  });
}

// "나" (the first post_as identity) is listed first and preselected.
function openMake() {
  $('mkForm').reset();
  S.mk = null;
  $('mkSlug').disabled = false;
  $('mkTitle').disabled = false;
  showFormError('mkError', null);
  const agents = Array.from(S.agents.values())
    .sort((a, b) => Number(b.agent_id === S.me) - Number(a.agent_id === S.me));
  const picks = agents.map((agent) => h('label', { class: 'pick' },
    h('input', { type: 'checkbox', value: agent.agent_id, checked: agent.agent_id === S.me }),
    av(agent.agent_id, 28),
    h('span', { class: 'who' }, agent.name || agent.agent_id, ' ', h('span', { class: 'id', text: agent.agent_id })),
    agent.agent_id === S.me ? h('span', { class: 'tag', text: '나' }) : null));
  if (picks.length === 0) picks.push(h('p', { class: 'muted flush', text: '등록된 에이전트가 없습니다' }));
  $('mkPicks').replaceChildren(...picks);
  $('mk').showModal();
  $('mkSlug').focus();
}

// Creates the room, then invites each pick. When an invite fails the room
// already exists: the form keeps it, and submitting again only sends the
// invites still missing.
async function submitMake() {
  if (S.mkBusy) return;
  const roomId = `room:${$('mkSlug').value.trim()}`;
  const title = $('mkTitle').value.trim();
  const picks = Array.from($('mkPicks').querySelectorAll('input:checked'), (input) => input.value);
  S.mkBusy = true;
  $('mkGo').disabled = true;
  showFormError('mkError', null);
  try {
    if (!S.mk) {
      const body = { room_id: roomId };
      if (title) body.title = title;
      await request('POST', API.rooms(), { json: body });
      S.mk = { roomId, invited: new Set() };
      $('mkSlug').disabled = true;
      $('mkTitle').disabled = true;
    }
    for (const id of picks) {
      if (S.mk.invited.has(id)) continue;
      await request('PUT', API.roomMember(S.mk.roomId, id));
      S.mk.invited.add(id);
    }
  } catch (error) {
    const created = S.mk ? ' (방은 만들었습니다. 다시 누르면 남은 초대만 보냅니다.)' : '';
    showFormError('mkError', `${failureText(asFailure(error))}${created}`);
    return;
  } finally {
    S.mkBusy = false;
    $('mkGo').disabled = false;
  }
  const created = S.mk.roomId;
  S.mk = null;
  $('mk').close();
  await refreshRooms();
  openRoom(created);
}

// A dialog closed half way (the room made, an invite failed) still shows
// the new room in the list.
function onMakeClosed() {
  if (!S.mk) return;
  S.mk = null;
  refreshRooms();
}

// --------------------------------------------------------------- directory

function renderDir() {
  const agents = Array.from(S.agents.values());
  $('dirCount').textContent = `에이전트 ${agents.length}`;
  $('dirBody').replaceChildren(...agents.map((agent) => h('tr', agent.agent_id === S.me || !S.me ? null : { data: { act: 'dm-open', id: agent.agent_id }, title: `${agent.name || agent.agent_id}와의 DM 열기` },
    h('td', null, h('span', { class: 'cell' }, avDot(agent.agent_id, 28), h('span', null, agent.name || agent.agent_id, ' ', h('span', { class: 'id', text: agent.agent_id })))),
    h('td', { text: agent.profile.role || NONE_TEXT }),
    h('td', { text: agent.joined_all ? '예' : '아니요' }),
    h('td', { class: 'pres' }, presenceLine(agent)),
    h('td', null, pill(agent)),
    h('td', { class: 'act' }, S.postAs.includes(agent.agent_id)
      ? h('span', { class: 'pill none', title: 'owner 게시 명의([owner] post_as)라 지울 수 없습니다', text: '보호됨' })
      : h('button', { class: 'btn sm ghost-bad', data: { act: 'del-open', id: agent.agent_id }, text: '삭제' })))));
}

// ------------------------------------------------- registration, deletion

function openRegister() {
  $('regForm').reset();
  showFormError('regError', null);
  $('reg').showModal();
  $('regId').focus();
}

function showFormError(id, text) {
  $(id).hidden = !text;
  $(id).textContent = text || '';
}

async function submitRegister() {
  if (S.regBusy) return;
  const body = { agent_id: $('regId').value.trim(), name: $('regName').value.trim() };
  const receiverUrl = $('regUrl').value.trim();
  if (receiverUrl) body.receiver_url = receiverUrl;
  S.regBusy = true;
  $('regGo').disabled = true;
  showFormError('regError', null);
  let created;
  try {
    created = await request('POST', API.agents(), { json: body });
  } catch (error) {
    showFormError('regError', failureText(asFailure(error)));
    return;
  } finally {
    S.regBusy = false;
    $('regGo').disabled = false;
  }
  $('reg').close();
  showCredentials(created);
}

// The response is shown once and kept nowhere else: the dialog's text is
// the only copy, and closing the dialog (button, Escape or logout) wipes it.
function showCredentials(created) {
  S.credFor = created.agent_id;
  $('credId').textContent = created.agent_id;
  $('credKey').textContent = created.agent_api_key;
  $('credSecret').textContent = created.receiver_secret;
  for (const button of $('cred').querySelectorAll('[data-act="copy"]')) button.textContent = '복사';
  $('cred').showModal();
}

async function onCredentialsClosed() {
  $('credKey').textContent = '';
  $('credSecret').textContent = '';
  const id = S.credFor;
  S.credFor = null;
  if (!id || !S.token) return;
  await refreshDirectory();
  renderDir();
  if (S.me && agentOf(id)) openDm(id);
}

// The Clipboard API needs a secure context, which a plain-HTTP tailnet origin
// is not; then the value is selected for a manual copy instead.
async function copySecret(button) {
  const node = $(button.dataset.k);
  try {
    await navigator.clipboard.writeText(node.textContent);
    button.textContent = '복사됨';
    return;
  } catch (_) {
    window.getSelection().selectAllChildren(node);
  }
  button.textContent = document.execCommand('copy') ? '복사됨' : '선택됨';
}

function openDelete(id) {
  S.delTarget = id;
  $('delId').textContent = id;
  const agent = agentOf(id);
  $('delWho').replaceChildren(av(id, 36), h('div', null, h('div', { class: 'nm', text: (agent && agent.name) || id }), h('div', { class: 'id', text: id })));
  $('delInput').value = '';
  $('delGo').disabled = true;
  showFormError('delError', null);
  $('del').showModal();
  $('delInput').focus();
}

function onDeleteInput() {
  $('delGo').disabled = S.delBusy || $('delInput').value.trim() !== S.delTarget;
}

function deleteFailureText(id, failure) {
  if (failure.code === 'agent_protected') {
    return `${id}는 owner 게시 명의([owner] post_as)라 지울 수 없습니다. 설정에서 빼고 platformd를 다시 시작한 뒤 지우세요. (${failureText(failure)})`;
  }
  return failureText(failure);
}

async function confirmDelete() {
  const id = S.delTarget;
  if (S.delBusy || !id || $('delInput').value.trim() !== id) return;
  S.delBusy = true;
  onDeleteInput();
  try {
    await request('DELETE', API.agent(id));
  } catch (error) {
    showFormError('delError', deleteFailureText(id, asFailure(error)));
    return;
  } finally {
    S.delBusy = false;
    onDeleteInput();
  }
  $('del').close();
  await afterDelete(id);
}

// The agent's messages and rooms stay; a room it was in closes if open.
async function afterDelete(id) {
  forgetAvatar(id);
  S.delivery.delete(id);
  S.inbox.delete(id);
  S.extraRooms = S.extraRooms.filter((room) => !room.members.includes(id));
  const room = currentRoom();
  const closing = room && room.kind === 'dm' && room.members.includes(id);
  if (S.member === id) S.member = null;
  await refreshDirectory();
  await refreshRooms();
  renderDir();
  if (closing) await openRoom('all', false);
  else renderInfo();
}

function openDm(id) {
  if (!S.me || id === S.me) return;
  const roomId = dmRoomId(S.me, id);
  if (!allRooms().some((room) => room.room_id === roomId)) {
    S.extraRooms.push({
      room_id: roomId, kind: 'dm', title: null, members: [S.me, id].sort(),
      created_by: S.me, created_at: new Date().toISOString(), archived_at: null,
      last_event_seq: null, last_message_at: null, last_sender_id: null, message_count: 0,
    });
  }
  $('dir').close();
  openRoom(roomId);
}

// ------------------------------------------------------------ data refresh

async function refreshDirectory() {
  try {
    const directory = await request('GET', API.agents());
    S.agents = new Map(directory.items.map((item) => [item.agent_id, item]));
  } catch (_) {
    return;
  }
  // The reaction kinds ride the directory's cadence.
  const kindsChanged = await loadKinds();
  // The presence title changes with the state, the note and, for offline
  // agents, the coarse "n분 전" text, so it re-renders as time passes too.
  const signature = JSON.stringify(Array.from(S.agents.values()).map((agent) => [agent.agent_id, agent.name, agent.avatar && agent.avatar.sha256, presenceOf(agent).title]));
  ensureAvatars();
  if (signature === S.directorySignature && !kindsChanged) return;
  S.directorySignature = signature;
  if ($('dir').open) renderDir();
  renderMe();
  renderTop();
  renderRooms();
  rerenderFeed();
  if (kindsChanged) rerenderThread();
  renderInfoPassively();
}

async function refreshRooms() {
  let list;
  try {
    list = await request('GET', API.rooms());
  } catch (_) {
    return;
  }
  const before = currentRoom();
  const wasArchived = isArchived(before);
  S.rooms = list.rooms;
  const known = new Set(S.rooms.map((room) => room.room_id));
  S.extraRooms = S.extraRooms.filter((room) => !known.has(room.room_id));
  const room = currentRoom();
  if (room && !document.hidden) markSeen(room);
  renderRooms();
  renderTop();
  if (room && isArchived(room) !== wasArchived) {
    // Archived elsewhere (the CLI): the composer and reply actions close.
    renderComposer();
    rerenderFeed();
  }
  if (room && roomKind(room) === 'topic') renderInfoPassively();
  loadSnippets();
  if (room && room.last_event_seq && seqNum(room.last_event_seq) > seqNum(newestSeq())) pollFeed();
}

async function refreshAll() {
  if (!S.token || document.hidden) return;
  await refreshDirectory();
  await refreshRooms();
  const id = infoAgentIdNow();
  if (id) refreshDelivery(id);
}

// ------------------------------------------------------------------ rooms

function resetRoomState() {
  S.feedSeq += 1;
  S.msgs = [];
  S.feedLoaded = false;
  S.feedError = null;
  S.hasOlder = false;
  S.olderCursor = null;
  S.olderBusy = false;
  S.newFrom = null;
  S.foldedSeq = '0';
  S.expanded.clear();
  S.reactCursor = null;
  S.reactError = null;
}

// `enter` shows the conversation on a phone; the first room after connecting
// leaves the phone on the room list, like the mock.
async function openRoom(id, enter = true) {
  const room = allRooms().find((candidate) => candidate.room_id === id);
  if (!room) return;
  const unread = unreadOf(room);
  S.room = id;
  S.q = '';
  $('histQ').value = '';
  $('top').classList.remove('searching');
  S.correcting = null;
  S.thread = null;
  S.edit = false;
  S.profileError = null;
  S.member = null;
  S.receiverEdit = null;
  S.actionError = null;
  S.sendError = null;
  S.quiet = false;
  S.roomError = null;
  S.infoTab = 'settings';
  resetRoomState();
  prefSet(PREF_ROOM, id);
  if (enter) app.classList.add('in-room');
  if (window.matchMedia('(max-width:1280px)').matches) app.classList.add('info-off');
  renderAll();
  await loadFeed();
  if (S.room !== id) return;
  if (unread > 0 && unread <= S.msgs.length) S.newFrom = S.msgs[S.msgs.length - unread].message_id;
  markSeen(currentRoom());
  renderRooms();
  renderFeed();
  toBottom();
}

async function reloadRoom() {
  resetRoomState();
  renderFeed();
  await loadFeed();
  renderFeed();
  toBottom();
}

function renderAll() {
  renderStyle();
  renderMe();
  renderRooms();
  renderTop();
  renderFeed();
  renderComposer();
  renderInfo();
  renderThread();
}

// ---------------------------------------------------------------- session

function readStoredToken() {
  try {
    return window.sessionStorage.getItem(TOKEN_STORAGE_KEY) || '';
  } catch (_) {
    return '';
  }
}

function storeToken(token, keep) {
  try {
    if (keep) window.sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
    else window.sessionStorage.removeItem(TOKEN_STORAGE_KEY);
  } catch (_) {
    // Storage may be unavailable (private mode); the token stays in memory.
  }
}

function loadSeen() {
  try {
    const parsed = JSON.parse(prefGet(PREF_SEEN));
    return parsed && typeof parsed === 'object' ? parsed : null;
  } catch (_) {
    return null;
  }
}

function showGate(message) {
  app.hidden = true;
  $('gate').hidden = false;
  $('loginError').hidden = !message;
  $('loginError').textContent = message || '';
  $('tokenInput').focus();
}

async function connect(token, keep) {
  S.session += 1;
  S.token = token;
  let owner;
  try {
    owner = await request('GET', API.owner());
  } catch (error) {
    const failure = asFailure(error);
    S.token = '';
    storeToken('', false);
    showGate(`연결 실패 — ${failure.status ? `${failure.status} ` : ''}${failureText(failure)}`);
    return;
  }
  storeToken(token, keep);
  S.postAs = owner.post_as;
  S.me = owner.post_as[0] || null;
  $('gate').hidden = true;
  app.hidden = false;
  await refreshDirectory();
  await refreshRooms();
  const seen = loadSeen();
  if (seen) {
    S.seen = seen;
  } else {
    for (const room of S.rooms) S.seen[room.room_id] = room.message_count;
    prefSet(PREF_SEEN, JSON.stringify(S.seen));
  }
  const remembered = prefGet(PREF_ROOM);
  const start = allRooms().some((room) => room.room_id === remembered) ? remembered : 'all';
  await openRoom(start, false);
  stopTimers();
  S.timers.push(window.setInterval(pollFeed, POLL_FEED_MS), window.setInterval(refreshAll, POLL_ROOMS_MS));
}

function stopTimers() {
  for (const timer of S.timers) window.clearInterval(timer);
  S.timers = [];
}

function logout(message) {
  stopTimers();
  S.session += 1;
  S.token = '';
  for (const dialog of document.querySelectorAll('dialog[open]')) dialog.close();
  storeToken('', false);
  for (const id of Array.from(S.avatars.keys())) forgetAvatar(id);
  S.agents = new Map();
  S.directorySignature = '';
  S.rooms = [];
  S.extraRooms = [];
  S.snippets.clear();
  S.quotes.clear();
  S.delivery.clear();
  S.inbox.clear();
  S.room = null;
  S.me = null;
  S.thread = null;
  S.correcting = null;
  S.quiet = false;
  S.roomError = null;
  S.mk = null;
  S.kinds = [];
  S.kindsSignature = '';
  resetRoomState();
  showGate(message || '');
}

// ----------------------------------------------------------------- events

function onClick(event) {
  if (!event.target.closest('.acts-open')) closeActions();
  const target = event.target.closest('[data-act]');
  if (!target) return;
  const id = target.dataset.id;
  const room = currentRoom();
  switch (target.dataset.act) {
    case 'room':
      openRoom(id);
      break;
    case 'back':
      app.classList.remove('in-room');
      break;
    case 'info':
      // The info panel and the thread panel share the right side.
      if (S.thread) {
        closeThread();
        app.classList.remove('info-off');
      } else {
        app.classList.toggle('info-off');
      }
      renderTop();
      break;
    case 'scrim':
      if (S.thread) closeThread();
      else app.classList.add('info-off');
      renderTop();
      break;
    case 'thread':
      openThread(target.dataset.k);
      break;
    case 'thread-close':
      closeThread();
      break;
    case 'thread-send':
      sendThread();
      break;
    case 'thread-reply':
      quoteInThread(target.dataset.k);
      break;
    case 'thread-correct':
      correctInThread(target.dataset.k);
      break;
    case 'thread-unquote':
      if (S.thread) S.thread.replyTo = null;
      renderThreadComposer();
      break;
    case 'thread-uncorrect':
      if (S.thread) S.thread.correcting = null;
      renderThreadComposer();
      break;
    case 'thread-dismiss':
      if (S.thread) S.thread.sendError = null;
      renderThreadComposer();
      break;
    case 'thread-mute':
      toggleThreadMute();
      break;
    case 'thread-older':
      loadOlderInThread();
      break;
    case 'correct':
      if (!canWrite()) break;
      S.correcting = target.dataset.k;
      renderComposer();
      $('input').focus();
      break;
    case 'uncorrect':
      S.correcting = null;
      renderComposer();
      break;
    case 'search':
      $('top').classList.toggle('searching');
      $('histQ').focus();
      break;
    case 'dir':
      renderDir();
      $('dir').showModal();
      $('dir').focus();
      break;
    case 'dir-close':
      $('dir').close();
      break;
    case 'dm-open':
      openDm(id);
      break;
    case 'reg-open':
      openRegister();
      break;
    case 'react':
      closeActions();
      toggleReaction(target.dataset.k, target.dataset.kind);
      break;
    case 'kinds-open':
      openKinds();
      break;
    case 'kinds-close':
      $('kinds').close();
      break;
    case 'kind-edit':
      editKind(id);
      break;
    case 'kind-toggle':
      toggleKind(id);
      break;
    case 'kind-new':
      resetKindForm();
      break;
    case 'reg-close':
      $('reg').close();
      break;
    case 'cred-close':
      $('cred').close();
      break;
    case 'copy':
      copySecret(target);
      break;
    case 'del-open':
      openDelete(id);
      break;
    case 'del-close':
      $('del').close();
      break;
    case 'mk-open':
      openMake();
      break;
    case 'mk-close':
      $('mk').close();
      break;
    case 'quiet':
      if (!canWrite()) break;
      S.quiet = !S.quiet;
      renderComposer();
      $('input').focus();
      break;
    case 'invite':
      if (room) inviteMember(room);
      break;
    case 'kick':
      if (room) removeMember(room, id);
      break;
    case 'archive':
      if (room) archiveRoom(room);
      break;
    case 'more': {
      const text = target.previousElementSibling;
      const folded = text.classList.toggle('clamp');
      target.textContent = folded ? '더 보기' : '접기';
      if (folded) S.expanded.delete(target.dataset.k);
      else S.expanded.add(target.dataset.k);
      break;
    }
    case 'jump':
      jumpTo(target.dataset.k);
      break;
    case 'reply':
      startReply(target.dataset.k);
      break;
    case 'dismiss-send':
      S.sendError = null;
      renderComposer();
      break;
    case 'send':
      send();
      break;
    case 'style':
      S.style = target.dataset.style || (S.style === 'bubble' ? 'flat' : 'bubble');
      prefSet(PREF_STYLE, S.style);
      renderStyle();
      renderFeed();
      toBottom();
      break;
    case 'tab':
      S.tab[room.room_id] = id;
      S.edit = false;
      S.profileError = null;
      S.receiverEdit = null;
      renderInfo();
      break;
    case 'member':
      S.member = id || null;
      S.edit = false;
      S.profileError = null;
      S.receiverEdit = null;
      renderInfo();
      break;
    case 'info-tab':
      S.infoTab = target.dataset.tab;
      if (S.infoTab === 'inbox') S.inbox.delete(infoAgentIdNow());
      renderInfo();
      break;
    case 'edit':
      S.edit = true;
      S.profileError = null;
      renderInfo();
      break;
    case 'edit-cancel':
      S.edit = false;
      S.profileError = null;
      renderInfo();
      break;
    case 'edit-save':
      saveProfile(id);
      break;
    case 'avatar-upload':
      S.avatarTarget = id;
      $('avatarFile').value = '';
      $('avatarFile').click();
      break;
    case 'avatar-clear':
      clearAvatar(id);
      break;
    case 'receiver-edit':
      S.receiverEdit = id;
      renderInfo();
      $('recvInput').focus();
      break;
    case 'receiver-cancel':
      S.receiverEdit = null;
      renderInfo();
      break;
    case 'receiver-save':
      saveReceiver(id);
      break;
    case 'receiver-clear':
      clearReceiver(id);
      break;
    case 'retry':
      retryDelivery(id);
      break;
    case 'join':
      toggleJoin(id);
      break;
    case 'inbox-refresh':
      loadInbox(id, false);
      break;
    case 'inbox-more':
      loadInbox(id, true);
      break;
    case 'logout':
      logout('');
      break;
    default:
      break;
  }
}

let searchTimer = 0;
function onSearch(event) {
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => {
    const query = event.target.value.trim();
    if (query === S.q) return;
    S.q = query;
    reloadRoom();
  }, SEARCH_DEBOUNCE_MS);
}

function onKeydown(event) {
  if (event.key !== 'Escape') return;
  closeActions();
  const thread = S.thread;
  if (thread && (thread.replyTo || thread.correcting)) {
    thread.replyTo = null;
    thread.correcting = null;
    renderThreadComposer();
  } else if (S.correcting) {
    S.correcting = null;
    renderComposer();
  } else if (thread && !document.querySelector('dialog[open]')) {
    closeThread();
  } else if (S.receiverEdit) {
    S.receiverEdit = null;
    renderInfo();
  }
}

function init() {
  S.style = prefGet(PREF_STYLE) === 'flat' ? 'flat' : 'bubble';
  if (window.matchMedia('(max-width:1280px)').matches) app.classList.add('info-off');
  renderStyle();
  document.addEventListener('click', onClick);
  document.addEventListener('keydown', onKeydown);
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) refreshAll();
  });
  $('roomQ').addEventListener('input', renderRooms);
  $('histQ').addEventListener('input', onSearch);
  $('input').addEventListener('input', grow);
  $('input').addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      send();
    }
  });
  $('threadInput').addEventListener('input', growThread);
  $('threadInput').addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      sendThread();
    }
  });
  bindSwipe();
  feed.addEventListener('scroll', () => {
    if (feed.scrollTop < OLDER_TRIGGER_PX) loadOlder();
  });
  $('avatarFile').addEventListener('change', (event) => {
    const file = event.target.files[0];
    if (file && S.avatarTarget) uploadAvatar(S.avatarTarget, file);
  });
  $('regForm').addEventListener('submit', (event) => {
    event.preventDefault();
    submitRegister();
  });
  $('delForm').addEventListener('submit', (event) => {
    event.preventDefault();
    confirmDelete();
  });
  $('delInput').addEventListener('input', onDeleteInput);
  $('mkForm').addEventListener('submit', (event) => {
    event.preventDefault();
    submitMake();
  });
  $('mk').addEventListener('close', onMakeClosed);
  $('kindForm').addEventListener('submit', (event) => {
    event.preventDefault();
    submitKind();
  });
  $('cred').addEventListener('close', onCredentialsClosed);
  $('loginForm').addEventListener('submit', (event) => {
    event.preventDefault();
    const token = $('tokenInput').value.trim();
    $('tokenInput').value = '';
    if (token) connect(token, $('tokenKeep').checked);
  });
  const stored = readStoredToken();
  if (stored) {
    $('tokenKeep').checked = true;
    connect(stored, true);
  } else {
    showGate('');
  }
}

init();
