# RFC 8785 test vectors

`../rfc8785_vectors.rs` canonicalises each file in `input/` with
`cs_core::event::canonical_json` and expects the bytes of the file with the same
name in `output/` (R4.7). The files must stay byte-for-byte as published:
`input/` is deliberately not canonical, and `output/` has no trailing newline.
That is why `biome.json` here keeps Biome from formatting or linting them.

## Source and licence

- Written by Anders Rundgren for the JSON Canonicalization Scheme, published in
  <https://github.com/cyberphone/json-canonicalization> under `testdata/`.
  Copyright 2018 Anders Rundgren, licensed under the Apache License, Version
  2.0; the full text is in [`LICENSE-APACHE`](LICENSE-APACHE). Upstream's
  notice reads:

  > Licensed under the Apache License, Version 2.0 (the "License"); you may not
  > use this file except in compliance with the License. You may obtain a copy
  > of the License at https://www.apache.org/licenses/LICENSE-2.0
  >
  > Unless required by applicable law or agreed to in writing, software
  > distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
  > WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
  > License for the specific language governing permissions and limitations
  > under the License.

- Copied unchanged from the `testdata/input` and `testdata/output` directories
  of the canon-json 0.2.1 crate (MIT OR Apache-2.0,
  <https://github.com/containers/canon-json-rs>), the serialiser these vectors
  check. `LICENSE-APACHE` is that crate's copy of the licence text.

## Difference from upstream

canon-json's `values.json` omits one number that upstream's has:
`333333333.33333329` in `input/`, `333333333.3333333` in `output/`. serde_json
without its `float_roundtrip` feature parses that literal to a neighbouring
double (it canonicalises as `333333333.33333325`), so the failure would be in
parsing, not in canonicalisation. `upstream_number_dropped_from_values_json_formats_as_published`
checks the formatter on that double directly. Event bodies never contain
floats (`check_body_numbers`), so this doesn't affect event hashes.
