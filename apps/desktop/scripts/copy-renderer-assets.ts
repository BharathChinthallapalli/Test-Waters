// Copies the renderer's static files next to its compiled scripts, so
// dist/renderer is the one directory the app:// handler serves.
import { copyFile, mkdir } from "node:fs/promises";

const source = new URL("../src/renderer/", import.meta.url);
const target = new URL("../dist/renderer/", import.meta.url);

await mkdir(target, { recursive: true });
for (const name of ["index.html", "style.css"]) {
  await copyFile(new URL(name, source), new URL(name, target));
}
