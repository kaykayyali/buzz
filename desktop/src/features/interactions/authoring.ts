import { KIND_INTERACTION_PROMPT } from "@/shared/constants/kinds";

/**
 * Desktop authoring for experimental interaction prompts (kind 40010).
 *
 * Mirrors the relay's schema rules in `buzz-core/src/interaction.rs` so a
 * person sees a precise message before signing instead of a relay rejection
 * after. The relay remains authoritative; this only builds the unsigned event.
 */

export type PromptType = "buttons" | "poll" | "form";
export type OptionStyle = "default" | "primary" | "danger";
export type FieldType = "text" | "number" | "select" | "boolean" | "date";
export type CloseRule = "first" | "manual" | "expiry" | `quorum:${number}`;
export type ResponderRule = "members" | "role:owner" | "role:admin";

export type DraftOption = {
  /** Stable React key for the editor row; not part of the event. */
  key?: string;
  id: string;
  label: string;
  style: OptionStyle;
  /** Set once a person edits the ID by hand so label edits stop rewriting it. */
  idEdited?: boolean;
};
export type DraftField = {
  key?: string;
  id: string;
  label: string;
  type: FieldType;
  required: boolean;
  /** Comma-separated select values, e.g. "60s, 6min"; ignored otherwise. */
  choices: string;
  idEdited?: boolean;
};

export type PromptDraft = {
  type: PromptType;
  text: string;
  options: DraftOption[];
  fields: DraftField[];
  closes: CloseRule;
  responders: ResponderRule;
  min: number;
  max: number;
  /** Seconds from now until the response deadline. */
  expiresIn: number;
};

export type UnsignedPrompt = {
  kind: number;
  content: string;
  tags: string[][];
};

export const MAX_LIFETIME_SECONDS = 30 * 24 * 60 * 60;
export const EXPIRY_PRESETS: { label: string; seconds: number }[] = [
  { label: "1 hour", seconds: 3600 },
  { label: "4 hours", seconds: 4 * 3600 },
  { label: "24 hours", seconds: 86_400 },
  { label: "3 days", seconds: 3 * 86_400 },
  { label: "7 days", seconds: 7 * 86_400 },
];

const IDENTIFIER = /^[A-Za-z0-9_.-]{1,64}$/;
// Matches the relay's `label` check: non-blank, at most 256 bytes, no control characters.
// biome-ignore lint/suspicious/noControlCharactersInRegex: mirrors the relay's char::is_control rejection
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

export function isIdentifier(value: string): boolean {
  return IDENTIFIER.test(value);
}

export function isLabel(value: string): boolean {
  return (
    value.trim().length > 0 &&
    new TextEncoder().encode(value).length <= 256 &&
    !CONTROL.test(value)
  );
}

/**
 * Derive a stable ASCII identifier from a label, unique among `taken`
 * (compared case-insensitively, as the relay does).
 */
export function suggestId(label: string, taken: readonly string[]): string {
  const base =
    label
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9_.-]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 48) || "option";
  const lower = new Set(taken.map((t) => t.toLowerCase()));
  if (!lower.has(base)) return base;
  for (let n = 2; n < 1000; n++) {
    const candidate = `${base}-${n}`;
    if (!lower.has(candidate)) return candidate;
  }
  return `${base}-${Date.now()}`;
}

export function emptyDraft(type: PromptType = "buttons"): PromptDraft {
  return {
    type,
    text: "",
    options:
      type === "form"
        ? []
        : type === "poll"
          ? [
              { key: "a", id: "a", label: "", style: "default" },
              { key: "b", id: "b", label: "", style: "default" },
            ]
          : [
              {
                key: "approve",
                id: "approve",
                label: "Approve",
                style: "primary",
              },
              { key: "deny", id: "deny", label: "Deny", style: "danger" },
            ],
    fields:
      type === "form"
        ? [
            {
              key: "answer",
              id: "answer",
              label: "Answer",
              type: "text",
              required: true,
              choices: "",
            },
          ]
        : [],
    closes: type === "poll" ? "expiry" : "first",
    responders: "members",
    min: type === "form" ? 0 : 1,
    max: type === "form" ? 0 : 1,
    expiresIn: 86_400,
  };
}

/** A validation failure with a message written for the person editing the draft. */
export class PromptDraftError extends Error {}

function fail(message: string): never {
  throw new PromptDraftError(message);
}

function parseSelectChoices(field: DraftField): string[] {
  const values = field.choices
    .split(",")
    .map((v) => v.trim())
    .filter((v) => v.length > 0);
  if (values.length < 1 || values.length > 12)
    fail(`"${field.label || field.id}" needs 1 to 12 comma-separated choices.`);
  const seen = new Set<string>();
  for (const value of values) {
    if (!isIdentifier(value))
      fail(`Choice "${value}" must use only letters, digits, "_", "-" or ".".`);
    if (seen.has(value)) fail(`Choice "${value}" is listed twice.`);
    seen.add(value);
  }
  return values;
}

/**
 * Build the unsigned prompt for `channelId`, throwing `PromptDraftError` with
 * an actionable message when the draft would be rejected by the relay.
 */
export function buildPrompt(
  draft: PromptDraft,
  channelId: string,
  now = Math.floor(Date.now() / 1000),
): UnsignedPrompt {
  if (
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
      channelId,
    )
  )
    fail("Open a channel before asking for a decision.");
  const text = draft.text.trim();
  if (!text) fail("Write the question first.");
  if (new TextEncoder().encode(text).length > 16_384)
    fail("The question is longer than 16 KiB.");

  const options = draft.options.map((o) => ({
    id: o.id.trim(),
    label: o.label.trim(),
    style: o.style,
  }));
  const ids = new Set<string>();
  const labels = new Set<string>();
  for (const option of options) {
    if (!isIdentifier(option.id))
      fail(
        `Option ID "${option.id}" must be 1–64 letters, digits, "_", "-" or ".".`,
      );
    if (!isLabel(option.label))
      fail(`Option "${option.id}" needs a label of at most 256 characters.`);
    const id = option.id.toLowerCase();
    const label = option.label.toLowerCase();
    if (ids.has(id)) fail(`Option ID "${option.id}" is used twice.`);
    if (labels.has(label))
      fail(`Option label "${option.label}" is used twice.`);
    ids.add(id);
    labels.add(label);
  }
  for (const a of options) {
    if (
      options.some(
        (b) => a.id !== b.id && a.label.toLowerCase() === b.id.toLowerCase(),
      )
    )
      fail(
        `Option label "${a.label}" matches another option's ID, which would make text replies ambiguous.`,
      );
  }

  const fields = draft.fields.map((f) => ({
    ...f,
    id: f.id.trim(),
    label: f.label.trim(),
  }));
  const fieldIds = new Set<string>();
  for (const field of fields) {
    if (!isIdentifier(field.id))
      fail(
        `Field ID "${field.id}" must be 1–64 letters, digits, "_", "-" or ".".`,
      );
    if (!isLabel(field.label))
      fail(`Field "${field.id}" needs a label of at most 256 characters.`);
    if (fieldIds.has(field.id)) fail(`Field ID "${field.id}" is used twice.`);
    fieldIds.add(field.id);
  }

  switch (draft.type) {
    case "buttons":
      if (options.length < 1 || options.length > 8)
        fail("Buttons need 1 to 8 options.");
      break;
    case "poll":
      if (options.length < 2 || options.length > 12)
        fail("A poll needs 2 to 12 options.");
      if (fields.length > 0) fail("A poll cannot carry form fields.");
      if (draft.closes === "first" || draft.closes.startsWith("quorum:"))
        fail("A poll closes manually or at its deadline.");
      break;
    case "form":
      if (fields.length < 1) fail("A form needs at least one field.");
      if (options.length > 0) fail("A form cannot carry options.");
      break;
  }
  if (fields.length > 12) fail("At most 12 fields are allowed.");

  const min = draft.type === "form" ? 0 : draft.min;
  const max = draft.type === "form" ? 0 : draft.max;
  if (
    !Number.isInteger(min) ||
    !Number.isInteger(max) ||
    min < 0 ||
    min > max ||
    max > options.length
  )
    fail(
      "The number of choices must be between the minimum and the option count.",
    );
  if (draft.type === "buttons" && (min !== 1 || max !== 1))
    fail("Buttons take exactly one choice.");
  if (draft.type === "poll" && min < 1)
    fail("A poll needs at least one choice per answer.");

  const quorum = draft.closes.startsWith("quorum:")
    ? Number(draft.closes.slice("quorum:".length))
    : null;
  if (
    quorum !== null &&
    (!Number.isInteger(quorum) || quorum < 1 || quorum > 256)
  )
    fail("A quorum must be between 1 and 256 responders.");
  if (!["members", "role:owner", "role:admin"].includes(draft.responders))
    fail("Choose who may answer.");
  if (
    !Number.isInteger(draft.expiresIn) ||
    draft.expiresIn < 1 ||
    draft.expiresIn > MAX_LIFETIME_SECONDS
  )
    fail("The deadline must be between one second and 30 days from now.");

  const tags: string[][] = [
    ["h", channelId],
    ["itype", draft.type],
    ["responders", draft.responders],
    ["visibility", "public"],
    ["closes", draft.closes],
    ["deadline", String(now + draft.expiresIn)],
  ];
  for (const option of options) {
    tags.push(
      option.style === "default"
        ? ["opt", option.id, option.label]
        : ["opt", option.id, option.label, option.style],
    );
  }
  for (const field of fields) {
    tags.push([
      "field",
      field.id,
      field.label,
      field.type,
      field.required ? "required" : "optional",
    ]);
    if (field.type === "select") {
      for (const value of parseSelectChoices(field)) {
        tags.push(["optsel", field.id, value]);
      }
    }
  }
  if (draft.type !== "form") {
    tags.push(["min", String(min)], ["max", String(max)]);
  }
  if (tags.length > 512) fail("The prompt carries too many tags.");
  return { kind: KIND_INTERACTION_PROMPT, content: text, tags };
}
