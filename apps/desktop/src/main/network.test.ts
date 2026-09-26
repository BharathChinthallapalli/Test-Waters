import assert from "node:assert/strict";
import { test } from "node:test";
import {
  isAllowedRequestUrl,
  keepSessionOnMachine,
  type OnMachineSession,
} from "./network.ts";

test("only app:// URLs are allowed", () => {
  assert.equal(isAllowedRequestUrl("app://renderer/index.html"), true);
  for (const url of [
    "https://redirector.gvt1.com/edgedl/chrome/dict/en-us-10-1.bdic",
    "http://127.0.0.1:1234/rpc",
    "file:///etc/passwd",
    "data:text/plain,hi",
    "not a url",
  ]) {
    assert.equal(isAllowedRequestUrl(url), false, url);
  }
});

test("the session turns the spellchecker off and cancels off-machine requests", () => {
  let listener:
    | Parameters<OnMachineSession["webRequest"]["onBeforeRequest"]>[1]
    | undefined;
  let urls: string[] = [];
  let spellchecker: boolean | undefined;
  keepSessionOnMachine({
    setSpellCheckerEnabled: (enable) => {
      spellchecker = enable;
    },
    webRequest: {
      onBeforeRequest(filter, registered) {
        urls = filter.urls;
        listener = registered;
      },
    },
  });
  const decide = (url: string) => {
    let cancel: boolean | undefined;
    listener?.({ url }, (response) => (cancel = response.cancel));
    return cancel;
  };

  assert.equal(spellchecker, false);
  assert.deepEqual(urls, ["<all_urls>"]);
  assert.equal(decide("https://example.com/"), true);
  assert.equal(decide("app://renderer/style.css"), false);
});
