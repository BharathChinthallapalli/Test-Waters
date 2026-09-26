/** The parts of Electron's Session that permission hardening uses. */
export interface PermissionSession {
  setPermissionRequestHandler(
    handler: (
      webContents: unknown,
      permission: string,
      callback: (granted: boolean) => void,
    ) => void,
  ): void;
  setPermissionCheckHandler(handler: () => boolean): void;
  setDevicePermissionHandler(handler: () => boolean): void;
}

/**
 * The renderer needs no browser permissions, so every permission request,
 * permission check and device (HID, serial, USB) permission is denied. Checks
 * and requests are separate paths in Electron, so both need a handler.
 */
export function denyAllPermissions(session: PermissionSession): void {
  session.setPermissionRequestHandler((_webContents, _permission, callback) =>
    callback(false),
  );
  session.setPermissionCheckHandler(() => false);
  session.setDevicePermissionHandler(() => false);
}
