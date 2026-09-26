// Sandboxed preload scripts cannot use ES modules, and they get only a small
// polyfilled `require`, so this file is plain CommonJS with no imports or
// exports of its own.
const { contextBridge } = require("electron") as typeof import("electron");

const api: import("./api.ts").CallsheetApi = Object.freeze({});

contextBridge.exposeInMainWorld("callsheet", api);
