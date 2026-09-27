// Sandboxed preload scripts cannot use ES modules, and they get only a small
// polyfilled `require`, so this file is plain CommonJS with no imports or
// exports of its own. The channel names repeat `src/main/ipc.ts`; a test keeps
// them identical.
const { contextBridge, ipcRenderer } =
  require("electron") as typeof import("electron");

type DaemonStatus = import("../shared/daemon-status.ts").DaemonStatus;

const STATUS_GET_CHANNEL = "callsheet:status:get";
const STATUS_CHANGED_CHANNEL = "callsheet:status:changed";

const api: import("./api.ts").CallsheetApi = Object.freeze({
  status: Object.freeze({
    get: (): Promise<DaemonStatus> => ipcRenderer.invoke(STATUS_GET_CHANNEL),
    onChange: (listener: (status: DaemonStatus) => void): (() => void) => {
      if (typeof listener !== "function") {
        throw new TypeError("status.onChange needs a function");
      }
      // The IPC event object gives access to ipcRenderer, so only the status
      // is passed on (Electron security guide, item 20).
      const forward = (_event: unknown, status: DaemonStatus): void =>
        listener(status);
      ipcRenderer.on(STATUS_CHANGED_CHANNEL, forward);
      return () => {
        ipcRenderer.removeListener(STATUS_CHANGED_CHANNEL, forward);
      };
    },
  }),
});

contextBridge.exposeInMainWorld("callsheet", api);
