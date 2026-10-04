// Stylus's icons: one hand-drawn family, the only place an icon is defined. src/index.html inlines
// the same strings (icons.test.js keeps them equal), app.js imports them.
//
// The system: a 24×24 grid with a 2-3 px margin; line icons use a 1.75 stroke with round caps and
// joins, boxes have rounded corners. Solid only where the role needs weight: the transport (play,
// pause, previous, next) and the on state of the heart. Solid shapes round their corners with arcs,
// never with a stroke: the UI paints icons in translucent colours, and a stroke over a fill would
// show as a lighter rim. Each svg carries its own paint attributes, so it draws the same anywhere;
// CSS sets only its size and colour (currentColor). Library is a record in front of its sleeve.

const LINE = 'fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"';

const svg = (name, paint, body) => `<svg class="ic ic-${name}" viewBox="0 0 24 24" aria-hidden="true" ${paint}>${body}</svg>`;

const HEART = "M12 20c-.3 0-.55-.08-.8-.26C8.4 17.7 4 14.2 4 9.6 4 7.05 5.95 5 8.4 5c1.5 0 2.8.73 3.6 1.9C12.8 5.73 14.1 5 15.6 5 18.05 5 20 7.05 20 9.6c0 4.6-4.4 8.1-7.2 10.14-.25.18-.5.26-.8.26z";
const SPEAKER = "M4 10v4a1 1 0 0 0 1 1h2.6l4.15 3.6a.6.6 0 0 0 1-.45V5.85a.6.6 0 0 0-1-.45L7.6 9H5a1 1 0 0 0-1 1z";
const LOOP = "M4 11.5v-2a3 3 0 0 1 3-3h13M17 3.5l3 3-3 3M20 12.5v2a3 3 0 0 1-3 3H4M7 20.5l-3-3 3-3";

export const ICONS = {
  // the topbar
  library: svg("library", LINE, '<path d="M7.46 15.5H5a2 2 0 0 1-2-2v-8a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v.57"/><circle cx="14" cy="13" r="7"/><circle cx="14" cy="13" r="2"/>'),
  search: svg("search", LINE, '<circle cx="10.5" cy="10.5" r="6.5"/><path d="m15.25 15.25 4.75 4.75"/>'),
  settings: svg(
    "settings",
    LINE,
    '<path d="M10.33 5.3l.26-2.19h2.82l.26 2.19a6.9 6.9 0 0 1 1.88.79l1.74-1.37 1.99 1.99-1.37 1.74a6.9 6.9 0 0 1 .79 1.88l2.19.26v2.82l-2.19.26a6.9 6.9 0 0 1-.79 1.88l1.37 1.74-1.99 1.99-1.74-1.37a6.9 6.9 0 0 1-1.88.79l-.26 2.19h-2.82l-.26-2.19a6.9 6.9 0 0 1-1.88-.79l-1.74 1.37-1.99-1.99 1.37-1.74a6.9 6.9 0 0 1-.79-1.88l-2.19-.26v-2.82l2.19-.26a6.9 6.9 0 0 1 .79-1.88L4.72 6.71l1.99-1.99 1.74 1.37a6.9 6.9 0 0 1 1.88-.79z"/><circle cx="12" cy="12" r="2.75"/>',
  ),
  chevronDown: svg("chevron-down", LINE, '<path d="m6.5 9.25 5.5 5.5 5.5-5.5"/>'),

  // the transport
  play: svg("play", 'fill="currentColor"', '<path d="M7.5 5.25v13.5a.9.9 0 0 0 1.38.76l10.6-6.75a.9.9 0 0 0 0-1.52L8.88 4.49A.9.9 0 0 0 7.5 5.25z"/>'),
  pause: svg("pause", 'fill="currentColor"', '<rect x="6" y="5" width="4.25" height="14" rx="1.5"/><rect x="13.75" y="5" width="4.25" height="14" rx="1.5"/>'),
  previous: svg("previous", 'fill="currentColor"', '<path d="M18.5 6.4v11.2a.9.9 0 0 1-1.4.75l-8.3-5.6a.9.9 0 0 1 0-1.5l8.3-5.6a.9.9 0 0 1 1.4.75z"/><rect x="5" y="5.5" width="2.25" height="13" rx="1.125"/>'),
  next: svg("next", 'fill="currentColor"', '<path d="M5.5 6.4v11.2a.9.9 0 0 0 1.4.75l8.3-5.6a.9.9 0 0 0 0-1.5L6.9 5.65a.9.9 0 0 0-1.4.75z"/><rect x="16.75" y="5.5" width="2.25" height="13" rx="1.125"/>'),
  shuffle: svg("shuffle", LINE, '<path d="M4 7h2.2c2 0 3.3.9 4.4 2.5l2.8 5c1.1 1.6 2.4 2.5 4.4 2.5H20M4 17h2.2c1.3 0 2.3-.4 3.1-1.1M14.7 8.1c.8-.7 1.8-1.1 3.1-1.1H20M17 4l3 3-3 3M17 14l3 3-3 3"/>'),
  repeat: svg("repeat", LINE, `<path d="${LOOP}"/>`),
  repeatOne: svg("repeat-one", LINE, `<path d="${LOOP}"/><path stroke-width="1.6" d="M10.9 10.4 12.4 9.4v5.2"/>`),
  heart: svg("heart", LINE, `<path d="${HEART}"/>`),
  heartFilled: svg("heart-filled", 'fill="currentColor"', `<path d="${HEART}"/>`),
  volumeHigh: svg("volume-high", LINE, `<path d="${SPEAKER}"/><path d="M15.75 9.5a3.5 3.5 0 0 1 0 5M18.25 7a7 7 0 0 1 0 10"/>`),
  volumeLow: svg("volume-low", LINE, `<path d="${SPEAKER}"/><path d="M15.75 9.5a3.5 3.5 0 0 1 0 5"/>`),
  mute: svg("mute", LINE, `<path d="${SPEAKER}"/><path d="m16 10 4 4M20 10l-4 4"/>`),
  playlist: svg("playlist", LINE, '<path d="M4 6.5h14M4 11.5h14M4 16.5h6.5"/><path fill="currentColor" stroke="none" d="M14.5 14.1v4.8a.5.5 0 0 0 .76.43l3.9-2.4a.5.5 0 0 0 0-.86l-3.9-2.4a.5.5 0 0 0-.76.43z"/>'),

  // lists and sheets
  addToQueue: svg("add-to-queue", LINE, '<path d="M4 6.5h14M4 11.5h9M4 16.5h6.5M17.5 13v7M14 16.5h7"/>'),
  close: svg("close", LINE, '<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/>'),
  back: svg("back", LINE, '<path d="M14.75 5.5 8.25 12l6.5 6.5"/>'),
  plus: svg("plus", LINE, '<path d="M12 5.5v13M5.5 12h13"/>'),

  // the current cover: turn the sleeve over (close turns it back)
  info: svg("info", LINE, '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5.25"/><circle cx="12" cy="7.75" r="1.1" fill="currentColor" stroke="none"/>'),
};
