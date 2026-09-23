# fixtures/ — golden vector contract

Every file is a **vector suite**: canonical JSON, 2-space indent, UTF-8,
no trailing whitespace. These vectors are the **frozen contract** — they
were generated from the original implementation and are now authoritative
in their own right. **Regenerate only inside a deliberate protocol
revision; never hand-edit.** A vector that fails means the implementation
is wrong — fix the implementation, not the fixture.

## Suite envelope

```json
{
  "format_version": 1,
  "suite": "<domain>.<name>",
  "source": {
    "repo": "IGUNUBLUE/lerdr",
    "commit": "<full sha>",
    "package": "<go package or frontend path>",
    "generator": "<file name>"
  },
  "vectors": [ { "name": "<kebab-case>", "comment": "?", ... } ]
}
```

`generated_at` timestamps are forbidden (determinism — same code must
emit byte-identical files). All binary data is base64. All hex lowercase.

## Suites

| Dir | Suite | Vector shape (per entry) |
|---|---|---|
| `crypto/` | `crypto.handshake.{credential,invitation}` | `{name, auth_kind, client_ephemeral_pub_b64, server_ephemeral_pub_b64, transcript_hex, session_key_c2s_b64, session_key_s2c_b64, proof_client_b64, proof_server_b64, finish_frames[]}` — full stepping material |
| `crypto/` | `crypto.frames.{json,binary}` | `{name, direction:"c2s"\|"s2c", seq, plaintext_b64, sealed_frame_b64}` |
| `crypto/` | `crypto.failures` | `{name, base_vector:"<suite>#<name>", mutation:"<what was changed>", expected_error:"replay"\|"auth"\|"format"\|"seq"}` |
| `pane/` | `pane.delta` | `{name, previous, current, expected_segments:[{copy_start?,copy_lines?,text?}], expected_applied, efficient:bool}` |
| `pane/` | `pane.sendbuffer` | `{name, capacity_bytes, ops:[{op:"push",msg_type,size}|{op:"drain",count}|{op:"pop"}], expected:{evicted:[], pending_types:[], bytes}}` |
| `pane/` | `pane.lease` | `{name, initial_size:{cols,rows}, ops:[{op:"lease",client,cols,rows,owner_gone?}|{op:"release",client}|{op:"local_resize",cols,rows}|{op:"advance_seconds",n}|{op:"release_client",client}], expect_error?:"invalid_columns"\|"invalid_rows"\|"invalid_lease"\|"lease_owner_gone", expected_effective:{cols,rows}, expected_holders:[]}` — `expected_effective` = tty size after ops; `expected_holders` = sorted live-lease clients (released-in-grace still counts); `owner_gone` simulates a cancelled-ctx lease |
| `ansi/` | `ansi.spans` | `{name, input, expected_html, expected_spans:[{text,fg?,bg?,bold?,italic?,underline?,dim?,class?,href?,width_cells?,styles?}]}` — `expected_html` is authoritative (the parser produces HTML); spans are the derived model |
| `questions/` | `questions.interaction` | `{name, agent_kind?, pane_lines:[], expected_interaction:{kind,question,options[],other{...},submit_label,can_chat,can_go_back,...}}` |
| `conversation/` | `conversation.page.<agent>` | `{name, jsonl_lines:[], expected_entries:[{role,text,tools[]}], next_cursor}` |
| `protocol/` | `protocol.envelope` | `{name, type, direction, json}` — canonical serialization per outbound/inbound message type |

## Consumers

- `relay/tests/vectors/` (Rust harness) — decodes + asserts every suite.
- `:core:testing` fixture runner (Kotlin/JVM) — same files, same asserts.

## Rules for generators

1. Emit the envelope exactly; omit `generated_at`.
2. Cover the edges listed in `docs/10-spec-gaps.md` P0 items.
3. A suite with zero vectors fails CI — a missing generator is a bug.
4. Keep vectors small (<64KB per entry) — they are reviewed in PRs.
