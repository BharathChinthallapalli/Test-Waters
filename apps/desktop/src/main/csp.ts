/**
 * Content-Security-Policy for the renderer. It is sent as a response header by
 * the app:// protocol handler and repeated in renderer/index.html as a <meta>
 * tag; a test keeps the two identical. Scripts may only come from the app's
 * own origin: no inline scripts, no eval, no remote hosts.
 */
export const CONTENT_SECURITY_POLICY = [
  "default-src 'none'",
  "script-src 'self'",
  "style-src 'self'",
  "img-src 'self'",
  "base-uri 'none'",
  "form-action 'none'",
].join("; ");
