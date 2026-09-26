import type { BrowserWindowConstructorOptions } from "electron";

/**
 * Options for every Callsheet window. Kept in one pure function so a unit test
 * can assert each security flag without starting Electron.
 */
export function createWindowOptions(
  preloadPath: string,
): BrowserWindowConstructorOptions {
  return {
    width: 1100,
    height: 720,
    show: false,
    webPreferences: {
      preload: preloadPath,
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      nodeIntegrationInWorker: false,
      nodeIntegrationInSubFrames: false,
      webSecurity: true,
      allowRunningInsecureContent: false,
      experimentalFeatures: false,
      webviewTag: false,
      navigateOnDragDrop: false,
      // The built-in spellchecker downloads dictionaries from a Google CDN (#33).
      spellcheck: false,
    },
  };
}
