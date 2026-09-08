import assert from "node:assert/strict";
import test from "node:test";
import { parseInteractionPrompt, parseInteractionState } from "./protocol.ts";
const relay = "a".repeat(64);
const prompt = "b".repeat(64);
const channel = "00000000-0000-0000-0000-000000000001";
const state = {
  id: "c".repeat(64),
  pubkey: relay,
  kind: 39010,
  created_at: 1,
  tags: [
    ["d", prompt],
    ["h", channel],
  ],
  content: JSON.stringify({
    version: 1,
    revision: 2,
    status: "closed",
    close_reason: "first",
    winner: "yes",
    tally: { yes: 1 },
    responders: [
      {
        pubkey: "d".repeat(64),
        event_id: "e".repeat(64),
        created_at: 1,
        choices: ["yes"],
      },
    ],
  }),
  sig: "",
};
test("only matching relay state can authorize a decision card", () => {
  assert.equal(
    parseInteractionState(state, prompt, channel, relay)?.winner,
    "yes",
  );
  for (const patch of [
    { pubkey: "f".repeat(64) },
    { kind: 40011 },
    {
      tags: [
        ["d", "f".repeat(64)],
        ["h", channel],
      ],
    },
    {
      tags: [
        ["d", prompt],
        ["h", "other"],
      ],
    },
    { content: "{" },
    {
      content: JSON.stringify({
        version: 1,
        revision: -1,
        status: "open",
        responders: [],
        tally: {},
      }),
    },
  ])
    assert.equal(
      parseInteractionState({ ...state, ...patch }, prompt, channel, relay),
      null,
    );
});
test("unsupported form privacy and fields fail closed", () => {
  const event = {
    ...state,
    id: prompt,
    kind: 40010,
    tags: [
      ["h", channel],
      ["itype", "form"],
      ["field", "note", "Note", "text", "required"],
      ["deadline", "2000"],
    ],
    content: "Details?",
  };
  assert.equal(
    parseInteractionPrompt(event, prompt, channel).fields[0].type,
    "text",
  );
  assert.throws(() =>
    parseInteractionPrompt(
      { ...event, tags: [...event.tags, ["visibility", "asker-only"]] },
      prompt,
      channel,
    ),
  );
  assert.throws(() =>
    parseInteractionPrompt(
      {
        ...event,
        tags: [...event.tags, ["field", "key", "Key", "secret", "required"]],
      },
      prompt,
      channel,
    ),
  );
});

const base = {
  ...state,
  id: prompt,
  kind: 40010,
  content: "Question?",
};
const withTags = (tags) => ({ ...base, tags: [["h", channel], ...tags] });

test("prompt parsing applies the same per-type limits as the relay", () => {
  const buttons = withTags([
    ["itype", "buttons"],
    ["opt", "approve", "Approve", "primary"],
    ["opt", "deny", "Deny"],
    ["closes", "first"],
    ["deadline", "2000"],
  ]);
  const parsed = parseInteractionPrompt(buttons, prompt, channel);
  assert.equal(parsed.type, "buttons");
  assert.deepEqual(
    parsed.options.map((o) => o.style),
    ["primary", "default"],
  );
  assert.equal(parsed.min, 1);
  assert.equal(parsed.max, 1);
  assert.equal(parsed.deadline, 2000);
  const poll = withTags([
    ["itype", "poll"],
    ["opt", "a", "A"],
    ["opt", "b", "B"],
    ["min", "1"],
    ["max", "2"],
    ["closes", "manual"],
    ["deadline", "2000"],
  ]);
  assert.equal(parseInteractionPrompt(poll, prompt, channel).max, 2);
  const form = withTags([
    ["itype", "form"],
    ["field", "size", "Size", "select", "required"],
    ["optsel", "size", "s", "Small"],
    ["optsel", "size", "m"],
    ["deadline", "2000"],
  ]);
  const parsedForm = parseInteractionPrompt(form, prompt, channel);
  assert.equal(parsedForm.min, 0);
  assert.equal(parsedForm.max, 0);
  assert.deepEqual(parsedForm.fields[0].options, [
    { id: "s", label: "Small" },
    { id: "m", label: "m" },
  ]);
  for (const [name, event, id = prompt, ch = channel] of [
    ["id mismatch", buttons, "f".repeat(64)],
    ["channel mismatch", buttons, prompt, "other"],
    ["wrong kind", { ...buttons, kind: 9 }],
    ["missing h", { ...buttons, tags: buttons.tags.slice(1) }],
    [
      "unsupported itype",
      withTags([
        ["itype", "slider"],
        ["deadline", "2000"],
      ]),
    ],
    [
      "expiration on a prompt",
      withTags([
        ["itype", "buttons"],
        ["opt", "a", "A"],
        ["deadline", "2000"],
        ["expiration", "1"],
      ]),
    ],
    [
      "missing deadline",
      withTags([
        ["itype", "buttons"],
        ["opt", "a", "A"],
      ]),
    ],
    [
      "non-numeric deadline",
      withTags([
        ["itype", "buttons"],
        ["opt", "a", "A"],
        ["deadline", "soon"],
      ]),
    ],
    [
      "max above options",
      withTags([
        ["itype", "poll"],
        ["opt", "a", "A"],
        ["opt", "b", "B"],
        ["max", "3"],
        ["deadline", "2000"],
      ]),
    ],
    [
      "min above max",
      withTags([
        ["itype", "poll"],
        ["opt", "a", "A"],
        ["opt", "b", "B"],
        ["min", "2"],
        ["max", "1"],
        ["deadline", "2000"],
      ]),
    ],
    [
      "option without label",
      withTags([
        ["itype", "buttons"],
        ["opt", "a"],
        ["deadline", "2000"],
      ]),
    ],
    [
      "too many options",
      withTags([
        ["itype", "poll"],
        ...Array.from({ length: 13 }, (_, i) => [
          "opt",
          `o${i}`,
          `Option ${i}`,
        ]),
        ["deadline", "2000"],
      ]),
    ],
  ]) {
    assert.throws(() => parseInteractionPrompt(event, id, ch), undefined, name);
  }
});

test("state parsing fails closed on every malformed shape", () => {
  const good = JSON.parse(state.content);
  const summary = (patch) => ({
    ...state,
    content: JSON.stringify({ ...good, ...patch }),
  });
  assert.equal(
    parseInteractionState(summary({}), prompt, channel, relay)?.revision,
    2,
  );
  for (const [name, event] of [
    ["future version", summary({ version: 2 })],
    ["missing version", summary({ version: undefined })],
    ["unknown status", summary({ status: "paused" })],
    ["non-integer revision", summary({ revision: 1.5 })],
    ["responders not an array", summary({ responders: {} })],
    [
      "responder without choices",
      summary({ responders: [{ pubkey: "d".repeat(64), created_at: 1 }] }),
    ],
    [
      "responder without created_at",
      summary({ responders: [{ pubkey: "d".repeat(64), choices: [] }] }),
    ],
    ["missing tally", summary({ tally: undefined })],
    ["negative tally", summary({ tally: { yes: -1 } })],
    ["fractional tally", summary({ tally: { yes: 0.5 } })],
    ["array content", { ...state, content: "[]" }],
    ["missing d tag", { ...state, tags: [["h", channel]] }],
  ]) {
    assert.equal(
      parseInteractionState(event, prompt, channel, relay),
      null,
      name,
    );
  }
});
