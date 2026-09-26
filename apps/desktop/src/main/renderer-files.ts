import path from "node:path";

export const APP_SCHEME = "app";
export const RENDERER_HOST = "renderer";
export const RENDERER_URL = `${APP_SCHEME}://${RENDERER_HOST}/index.html`;

const CONTENT_TYPES: Readonly<Record<string, string>> = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

/**
 * Maps an app:// request URL to a file inside the renderer directory.
 * Returns null for any other host, for malformed percent-escapes, for paths that
 * escape the directory and for file types the renderer does not ship.
 */
export function resolveRendererFile(
  requestUrl: string,
  rendererDir: string,
): { filePath: string; contentType: string } | null {
  const url = new URL(requestUrl);
  if (url.protocol !== `${APP_SCHEME}:` || url.host !== RENDERER_HOST) {
    return null;
  }
  let relative: string;
  try {
    relative = decodeURIComponent(url.pathname).replace(/^\/+/, "");
  } catch {
    return null; // malformed percent-escape
  }
  const filePath = path.resolve(rendererDir, relative || "index.html");
  const fromRoot = path.relative(rendererDir, filePath);
  if (!fromRoot || fromRoot.startsWith("..") || path.isAbsolute(fromRoot)) {
    return null;
  }
  const contentType = CONTENT_TYPES[path.extname(filePath)];
  return contentType ? { filePath, contentType } : null;
}
