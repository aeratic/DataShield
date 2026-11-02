# DataShield storage standard

This document is the **interoperability contract** for DataShield vaults. A
`.vault` file **plus the master password** is the entire interchange — no key
ever travels. Any DataShield app (the TUI today, a GUI later) that implements
this document can open, edit and write the same files losslessly.

There are two independent version numbers:

| Layer | Where | Value | Bound into the AEAD tag? |
|---|---|---|---|
| **File format** | plaintext header (`version`) | **4** | yes — via the AAD string |
| **Content schema** (DCS) | inside the encrypted blob (`schema_version`) | **1** | no (it is authenticated as ciphertext) |

The **file version selects the parser** — there is no content sniffing. They
evolve independently: the envelope governs crypto/framing, the content schema is
the record model a GUI renders.

---

## 1. The envelope (file format 4)

A vault file is UTF-8 JSON:

```json
{
  "version": 4,
  "username": "bruno",
  "algorithm": "AES-256-GCM",
  "style": "Vanilla",
  "kdf": { "algorithm": "Argon2id", "m_cost_kib": 65536, "t_cost": 3, "p_cost": 1, "salt": "<base64>" },
  "nonce": "<base64>",
  "data": "<base64 AEAD ciphertext of the DCS document>"
}
```

Everything in the header is **plaintext metadata** (leaks nothing beyond what a
password manager must reveal: which AEAD/KDF, the salt, the nonce, ciphertext
size). The item list, secret values and custom-category registry are all inside
`data`.

**Key derivation (no key is ever stored).** The 32-byte vault key is derived
from `master password + salt` with the chosen KDF (`derive_key`). 128-bit AEADs
use the first 16 bytes. The KDF context strings `"datashield v3 vault key"`
(BLAKE3 derive-key context and HKDF `info`) are **frozen constants of this
standard** and must never change, or existing vaults become unopenable.

**AAD (header binding).** The header is bound as the AEAD associated data, so
tampering with it breaks the tag:

```
datashield-v{file_version}|u{len}:{username}|{algorithm}|{kdf.canonical()}|{salt_b64}
```

`{file_version}` is the file's **own** version (a v3 blob is verified against the
`datashield-v3` prefix; a v4 blob against `datashield-v4`). The username is
length-prefixed (`u{len}:`) because it is the one free-form field and may contain
the `|` delimiter.

**Sanity before derivation.** A reader must validate KDF parameters (sane cost
bounds), salt length (8–64 bytes) and nonce length (per the AEAD) **before**
running the KDF, so a crafted header can't request terabytes of memory.

**Decrypt = verify.** AEAD tag failure means *wrong password* — indistinguishable
from tampering by construction. There is no separate password hash to attack.

---

## 2. The content document (DCS-1)

`data` decrypts to a self-identifying, versioned JSON document:

```json
{
  "schema": "datashield.content",
  "schema_version": 1,
  "categories": [ /* CustomCategory… (custom only) */ ],
  "items":      [ /* Item… */ ]
}
```

A format-3 file instead holds a **bare JSON array** of legacy items; the header's
`version: 3` tells the reader to use the legacy parser and migrate (§5).

### 2.1 Item

```json
{
  "id": "8f14e45f-ceea-467f-9a4c-b9d1c3a7e210",
  "category": "credit_card",
  "label": "Visa Intesa",
  "fields": [
    { "key": "number", "type": "card_number", "value": "4111111111111111", "secret": true },
    { "key": "expiry", "type": "expiry",      "value": "09/28",            "secret": false }
  ],
  "created_at": "2026-07-11T09:30:00Z",
  "modified_at": null
}
```

- **`id`** — UUIDv4, lowercase, generated once, **immutable**. Every edit/delete
  targets `id`, never `label` (labels may duplicate).
- **`category`** — a category id (a reserved built-in, a custom id from
  `categories`, or an id this reader doesn't know). **Unknown ids must still
  render fully** — see the self-describing rule.
- **`label`** — display name in lists. Free text, non-unique.
- **`fields`** — an **ordered array**; the order is the display order and must be
  preserved on rewrite.
- **Timestamps** — RFC 3339 UTC (`…Z`) or `null`. `modified_at: null` = never
  modified; `created_at: null` = unknown (some migrated legacy items).

### 2.2 Field

```json
{ "key": "password", "type": "password", "label": "Master pw", "value": "…", "secret": true }
```

- **`key`** — machine key, `[a-z0-9_]{1,64}`, unique within the item.
- **`type`** — a rendering/validation hint (§2.3). **Unknown tokens are preserved
  verbatim on rewrite and rendered as `text`.** `type` is *never* a security
  property.
- **`label`** — optional. Absent ⇒ the app localizes the built-in `key`; present
  ⇒ display verbatim (custom fields store exactly what the user typed).
- **`value`** — **always a JSON string** (preserves leading zeros in PINs and
  card numbers; `\n` allowed for `note`).
- **`secret`** — bool, the **authoritative masking flag**. Missing ⇒ `false`.
  Masking follows `secret`, **never** `type`, so an unknown type can never leak.

**The load-bearing interop rule:** every field is self-describing (`type`,
`secret`, optional `label`). The category is only grouping/template metadata —
losing the registry loses nothing.

### 2.3 Field-type enumeration (DCS-1)

| token | value encoding | notes |
|---|---|---|
| `text` | as typed | the fallback for every unknown token |
| `note` | as typed | multiline (`\n` allowed) |
| `password` | as typed | |
| `pin` | as typed | leading zeros preserved (it is a string) |
| `email` · `username` · `url` · `phone` · `number` | as typed | |
| `date` | `YYYY-MM-DD` | |
| `expiry` | `MM/YY` | |
| `card_number` | as typed | |
| `totp` | base32 seed or `otpauth://` URI | |
| `image` | **base64 of the original file bytes** | *additive* — see below |

Writers should pre-set `secret: true` for `password`, `pin`, `totp`,
`card_number` and `image` (the user may override). New type tokens are
**additive** (no schema bump) thanks to the unknown-⇒-`text` rule.

**`image` (additive).** The value is the base64 (standard alphabet, padded) of
the file exactly as the user picked it — **never a re-encode**, so a scanned
document stays as legible as its source. The field carries its shape under the
`x_image` key of its `extra` map:

```json
{ "key": "photo_front", "type": "image", "value": "iVBORw0KGgo…", "secret": true,
  "x_image": { "mime": "image/png", "bytes": 40213 } }
```

`x_image.mime` is one of `image/png`, `image/jpeg`, `image/gif`; `x_image.bytes`
is the length of the ORIGINAL file (not of the base64). Both are display hints:
a reader that lacks them shows a size derived from the value's length. Writers
**must** set `secret: true` — that is what makes an older reader (which maps the
unknown `image` token to `text`) render eight bullets instead of the payload —
and must never render the value as text. Implementations should cap an import
(this one: 8 MiB) and bound every decode (this one: 4096 px per side).

### 2.4 Custom category

Built-in categories live in code (every app ships them, §3). Only **custom**
categories are stored, so they travel with the vault:

```json
{ "id": "streaming", "name": "Streaming",
  "fields": [ { "key": "profile", "type": "text", "label": "Profilo", "secret": false, "required": false } ],
  "created_at": "2026-07-11T09:00:00Z" }
```

- `id` — slug `[a-z0-9_]{1,64}`, unique per vault, must not shadow a reserved
  built-in id at creation. **Resolution rule at load time: a custom def with the
  same id takes precedence over a built-in**, so future built-in additions never
  break an existing vault.
- `fields` — the Add-form template (`FieldSpec`: `key`, `type`, optional `label`,
  `secret`, `required`). Editing a category later never rewrites existing items
  (they are self-describing).

### 2.5 App-private root members (`x_`-prefixed, additive)

Anything an app keeps that DCS-1 does not define lives under an `x_`-prefixed
member of the ROOT object, where §4.1's round-trip rule protects it: another
reader neither understands nor loses it, and `schema_version` stays 1. The ones
this app writes:

| member | what it is |
|---|---|
| `x_sync` | `{counter, device, updated_at}` — the monotonic replication stamp. Excluded from the content fingerprint (it *is* the stamp). |
| `x_memo` | `{messages: [ {id, ts, text, image?, vec?, vfp?} ]}` — the Memorandum. `text` may hold `\n`: one message is as many lines as were written into it. |
| `x_rag` | `{model, items: {"<item id>": {fp, vec}}}` — the item search index. |

`x_rag` is a **derived** side table: `model` names the embedding engine, `fp` is
a blake3 of the exact text that was embedded (the staleness signal), and `vec` is
that text's vector. A reader may drop the whole member — the next pass rebuilds
it. Two rules are normative for anyone who writes it:

- **only non-secret text may be embedded.** An embedding partially inverts, so a
  vector of a `secret: true` value would be that secret stored a second time, in
  a weaker form. Indexable text is the item label, its category, field
  keys/labels and the values of fields with `secret: false`.
- **vectors never leave the encrypted blob** — no sidecar, no index file, no
  cache. Same rule as `x_memo`'s `vec`.

---

## 3. Reserved built-in category ids

Shipped in code by every DataShield app, in this order:

`email` · `account` · `login` · `credit_card` · `api_key` · `note` · `ssh_key` ·
`wifi` · `identity`

Also reserved (not yet shipped, so custom ids may not use them): `database`
(the canonical *custom* example) and `bank_account`. Category names and built-in
field labels are localized per app; nothing localized is ever stored in the file.

---

## 4. Versioning & compatibility (normative)

1. Readers **must ignore unknown JSON members and round-trip them** (Rust:
   `#[serde(flatten)] extra` on the root, item, field and category; never
   `deny_unknown_fields`). Adding optional members does **not** bump
   `schema_version`.
2. `schema_version` bumps only on breaking shape changes. `schema_version` or
   file `version` **greater than this build knows ⇒ refuse** with a "created by a
   newer DataShield" error — never best-effort parse (a silently dropped member
   could lose a secret on the next save).
3. Writers always write file 4 / schema 1; readers accept file ∈ {3, 4}.
4. Never reuse or repurpose a `key`/`id`; deprecate instead.

---

## 5. Migration from format 3

Format 3 stored a bare array of `{name, email, password, when_created,
when_modified}`. On open (header `version: 3`) each record maps to a **`login`**
item:

| legacy | DCS-1 |
|---|---|
| — | `id` = fresh UUIDv4, `category` = `"login"` |
| `name` | `label` |
| `email` | field `{key:"email", type:"email", secret:false}` — omitted if empty |
| `password` | field `{key:"password", type:"password", secret:true}` — omitted if empty |
| `when_created` | `created_at`: strict parse of `Created on DD/MM/YYYY at HH:MM` → RFC 3339; else `null` |
| `when_modified` | `modified_at`: sentinel `"Not already modified."` → `null`; else parsed; else `null` |
| originals | kept verbatim in the item's `extra` as `x_legacy_when_created` / `x_legacy_when_modified` |

Legacy times were local wall-clock, migrated as-if-UTC: **ordering is preserved,
the offset is unknowable**. **Opening a v3 vault never rewrites it**; the first
mutating `save()` upgrades it to format 4 (same salt/key, fresh nonce, v4 AAD).

---

## 6. What a DataShield GUI must implement

1. Parse the header; enforce the sanity bounds **before** key derivation.
2. Derive the key exactly as §1 (including the frozen KDF context strings;
   128-bit AEADs use the first 16 key bytes).
3. Build the AAD exactly as §1 (the file's own version, length-prefixed
   username, canonical KDF string).
4. AEAD-decrypt (tag failure == wrong password). Parse DCS-1; **tolerate unknown
   members and unknown types; honor `secret`**.
5. Write back: fresh random nonce per save; atomic write (0600 temp + fsync +
   rename + dir fsync); preserve `extra`; keep ids stable; update `modified_at`.
6. Format 3: migrate as §5, or refuse with "open once in DataShield-TUI first".
