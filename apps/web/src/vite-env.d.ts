/// <reference types="vite/client" />

// Side-effect CSS imports (e.g. @xterm/xterm/css/xterm.css loaded inside the
// lazy Terminal chunk) have no runtime type. Declare the module so tsc accepts
// the dynamic import without pulling the stylesheet into the type graph.
declare module "*.css";
