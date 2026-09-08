import { Plus, X } from "lucide-react";
import * as React from "react";
import { toast } from "sonner";

import { relayClient } from "@/shared/api/relayClient";
import { signRelayEvent } from "@/shared/api/tauri";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import { Textarea } from "@/shared/ui/textarea";
import {
  buildPrompt,
  type CloseRule,
  type DraftField,
  type DraftOption,
  EXPIRY_PRESETS,
  emptyDraft,
  type FieldType,
  type OptionStyle,
  type PromptDraft,
  PromptDraftError,
  type PromptType,
  type ResponderRule,
  suggestId,
} from "./authoring";

const selectClass =
  "h-9 w-full rounded-md border border-input bg-background px-2 text-sm";
const TYPE_LABELS: Record<PromptType, string> = {
  buttons: "Buttons",
  poll: "Poll",
  form: "Form",
};
const FIELD_TYPES: FieldType[] = [
  "text",
  "number",
  "select",
  "boolean",
  "date",
];

function closeRules(type: PromptType): { value: CloseRule; label: string }[] {
  const manual = { value: "manual" as const, label: "When I close it" };
  const expiry = { value: "expiry" as const, label: "At the deadline" };
  if (type === "poll") return [expiry, manual];
  return [
    { value: "first", label: "On the first answer" },
    { value: "quorum:2", label: "After 2 answers" },
    { value: "quorum:3", label: "After 3 answers" },
    { value: "quorum:5", label: "After 5 answers" },
    manual,
    expiry,
  ];
}

/**
 * Compose, sign and publish an experimental interaction prompt (kind 40010)
 * into the active channel. The relay projects it as an ordinary message for
 * clients that do not render cards; see docs/experimental-interactions.md.
 */
export function AskInteractionDialog({
  channelId,
  open,
  onOpenChange,
}: {
  /** The channel this dialog publishes to; fixed for the dialog's lifetime. */
  channelId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [draft, setDraft] = React.useState<PromptDraft>(() => emptyDraft());
  const [error, setError] = React.useState<string | null>(null);
  const [pending, setPending] = React.useState(false);
  const submitting = React.useRef(false);
  const formId = React.useId();
  const fid = (name: string) => `${formId}-${name}`;

  const update = (patch: Partial<PromptDraft>) => {
    setError(null);
    setDraft((current) => ({ ...current, ...patch }));
  };
  const setType = (type: PromptType) => {
    setError(null);
    setDraft((current) => ({
      ...emptyDraft(type),
      text: current.text,
      responders: current.responders,
      expiresIn: current.expiresIn,
    }));
  };
  const updateOption = (index: number, patch: Partial<DraftOption>) =>
    update({
      options: draft.options.map((option, i) => {
        if (i !== index) return option;
        const next = { ...option, ...patch };
        if (patch.label !== undefined && !option.idEdited) {
          next.id = suggestId(
            patch.label,
            draft.options.filter((_, j) => j !== index).map((o) => o.id),
          );
        }
        return next;
      }),
    });
  const updateField = (index: number, patch: Partial<DraftField>) =>
    update({
      fields: draft.fields.map((field, i) => {
        if (i !== index) return field;
        const next = { ...field, ...patch };
        if (patch.label !== undefined && !field.idEdited) {
          next.id = suggestId(
            patch.label,
            draft.fields.filter((_, j) => j !== index).map((f) => f.id),
          );
        }
        return next;
      }),
    });

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (submitting.current) return;
    let unsigned: ReturnType<typeof buildPrompt>;
    try {
      unsigned = buildPrompt(draft, channelId);
    } catch (reason) {
      setError(
        reason instanceof PromptDraftError
          ? reason.message
          : "This request cannot be sent.",
      );
      return;
    }
    submitting.current = true;
    setPending(true);
    setError(null);
    try {
      const signed = await signRelayEvent(unsigned);
      await relayClient.publishEvent(
        signed,
        "Timed out sending the request.",
        "The relay did not accept the request.",
      );
      toast.success(draft.type === "poll" ? "Poll started" : "Request sent");
      onOpenChange(false);
      setDraft(emptyDraft(draft.type));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      submitting.current = false;
      setPending(false);
    }
  };

  const showOptions = draft.type !== "form";
  const showFields = draft.type !== "poll";
  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent
        className="max-h-[85vh] overflow-y-auto sm:max-w-[560px]"
        data-testid="ask-interaction-dialog"
      >
        <form onSubmit={(e) => void submit(e)} className="space-y-4">
          <DialogHeader>
            <DialogTitle>Ask for a decision</DialogTitle>
            <DialogDescription>
              Members answer with signed responses. Answers are visible to
              everyone who can read this channel. Experimental.
            </DialogDescription>
          </DialogHeader>

          <fieldset className="flex gap-2" disabled={pending}>
            <legend className="mb-1 text-sm">Kind of request</legend>
            {(Object.keys(TYPE_LABELS) as PromptType[]).map((type) => (
              <label
                key={type}
                className={`flex cursor-pointer items-center gap-1.5 rounded-md border px-3 py-1.5 text-sm ${draft.type === type ? "border-primary bg-primary/10" : "border-input"}`}
              >
                <input
                  type="radio"
                  name={fid("type")}
                  value={type}
                  checked={draft.type === type}
                  onChange={() => setType(type)}
                />
                {TYPE_LABELS[type]}
              </label>
            ))}
          </fieldset>

          <div className="space-y-1">
            <label htmlFor={fid("question")} className="text-sm">
              Question
            </label>
            <Textarea
              id={fid("question")}
              value={draft.text}
              disabled={pending}
              maxLength={16_384}
              rows={3}
              placeholder="Render episode 1? 12 stills, about 55 GPU minutes."
              onChange={(e) => update({ text: e.target.value })}
            />
          </div>

          {showOptions ? (
            <fieldset className="space-y-2" disabled={pending}>
              <legend className="text-sm">Options</legend>
              {draft.options.map((option, index) => (
                <div
                  key={option.key}
                  className="grid grid-cols-[1fr_minmax(0,7rem)_auto_auto] items-center gap-2"
                >
                  <Input
                    aria-label={`Option ${index + 1} label`}
                    value={option.label}
                    placeholder="Label"
                    onChange={(e) =>
                      updateOption(index, { label: e.target.value })
                    }
                  />
                  <Input
                    aria-label={`Option ${index + 1} ID`}
                    value={option.id}
                    placeholder="id"
                    className="font-mono text-xs"
                    onChange={(e) =>
                      updateOption(index, {
                        id: e.target.value,
                        idEdited: true,
                      })
                    }
                  />
                  {draft.type === "buttons" ? (
                    <select
                      aria-label={`Option ${index + 1} style`}
                      className={`${selectClass} w-auto`}
                      value={option.style}
                      onChange={(e) =>
                        updateOption(index, {
                          style: e.target.value as OptionStyle,
                        })
                      }
                    >
                      <option value="default">Default</option>
                      <option value="primary">Primary</option>
                      <option value="danger">Danger</option>
                    </select>
                  ) : (
                    <span />
                  )}
                  <Button
                    aria-label={`Remove option ${index + 1}`}
                    onClick={() =>
                      update({
                        options: draft.options.filter((_, i) => i !== index),
                      })
                    }
                    size="icon"
                    type="button"
                    variant="ghost"
                  >
                    <X />
                  </Button>
                </div>
              ))}
              <Button
                disabled={
                  draft.options.length >= (draft.type === "poll" ? 12 : 8)
                }
                onClick={() =>
                  update({
                    options: [
                      ...draft.options,
                      {
                        key: crypto.randomUUID(),
                        id: suggestId(
                          "option",
                          draft.options.map((o) => o.id),
                        ),
                        label: "",
                        style: "default",
                      },
                    ],
                  })
                }
                size="sm"
                type="button"
                variant="outline"
              >
                <Plus /> Add option
              </Button>
              {draft.type === "poll" ? (
                <div className="flex items-center gap-3 text-sm">
                  <label htmlFor={fid("min")}>Choose at least</label>
                  <Input
                    id={fid("min")}
                    type="number"
                    min={1}
                    className="w-20"
                    value={draft.min}
                    onChange={(e) => update({ min: Number(e.target.value) })}
                  />
                  <label htmlFor={fid("max")}>at most</label>
                  <Input
                    id={fid("max")}
                    type="number"
                    min={1}
                    className="w-20"
                    value={draft.max}
                    onChange={(e) => update({ max: Number(e.target.value) })}
                  />
                </div>
              ) : null}
            </fieldset>
          ) : null}

          {showFields ? (
            <fieldset className="space-y-2" disabled={pending}>
              <legend className="text-sm">
                {draft.type === "form" ? "Fields" : "Optional fields"}
              </legend>
              {draft.fields.map((field, index) => (
                <div
                  key={field.key}
                  className="space-y-2 rounded-md border p-2"
                >
                  <div className="grid grid-cols-[1fr_minmax(0,7rem)_auto_auto] items-center gap-2">
                    <Input
                      aria-label={`Field ${index + 1} label`}
                      value={field.label}
                      placeholder="Label"
                      onChange={(e) =>
                        updateField(index, { label: e.target.value })
                      }
                    />
                    <Input
                      aria-label={`Field ${index + 1} ID`}
                      value={field.id}
                      placeholder="id"
                      className="font-mono text-xs"
                      onChange={(e) =>
                        updateField(index, {
                          id: e.target.value,
                          idEdited: true,
                        })
                      }
                    />
                    <select
                      aria-label={`Field ${index + 1} type`}
                      className={`${selectClass} w-auto`}
                      value={field.type}
                      onChange={(e) =>
                        updateField(index, {
                          type: e.target.value as FieldType,
                        })
                      }
                    >
                      {FIELD_TYPES.map((type) => (
                        <option key={type} value={type}>
                          {type}
                        </option>
                      ))}
                    </select>
                    <Button
                      aria-label={`Remove field ${index + 1}`}
                      onClick={() =>
                        update({
                          fields: draft.fields.filter((_, i) => i !== index),
                        })
                      }
                      size="icon"
                      type="button"
                      variant="ghost"
                    >
                      <X />
                    </Button>
                  </div>
                  <div className="flex flex-wrap items-center gap-3 text-sm">
                    <label className="flex items-center gap-1.5">
                      <input
                        type="checkbox"
                        checked={field.required}
                        onChange={(e) =>
                          updateField(index, { required: e.target.checked })
                        }
                      />
                      Required
                    </label>
                    {field.type === "select" ? (
                      <Input
                        aria-label={`Field ${index + 1} choices`}
                        className="min-w-48 flex-1"
                        value={field.choices}
                        placeholder="Choices, comma-separated: 60s, 6min"
                        onChange={(e) =>
                          updateField(index, { choices: e.target.value })
                        }
                      />
                    ) : null}
                  </div>
                </div>
              ))}
              <Button
                disabled={draft.fields.length >= 12}
                onClick={() =>
                  update({
                    fields: [
                      ...draft.fields,
                      {
                        key: crypto.randomUUID(),
                        id: suggestId(
                          "field",
                          draft.fields.map((f) => f.id),
                        ),
                        label: "",
                        type: "text",
                        required: draft.type === "form",
                        choices: "",
                      },
                    ],
                  })
                }
                size="sm"
                type="button"
                variant="outline"
              >
                <Plus /> Add field
              </Button>
            </fieldset>
          ) : null}

          <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
            <div className="space-y-1">
              <label htmlFor={fid("closes")} className="text-sm">
                Closes
              </label>
              <select
                id={fid("closes")}
                className={selectClass}
                disabled={pending}
                value={draft.closes}
                onChange={(e) =>
                  update({ closes: e.target.value as CloseRule })
                }
              >
                {closeRules(draft.type).map((rule) => (
                  <option key={rule.value} value={rule.value}>
                    {rule.label}
                  </option>
                ))}
              </select>
            </div>
            <div className="space-y-1">
              <label htmlFor={fid("responders")} className="text-sm">
                Who can answer
              </label>
              <select
                id={fid("responders")}
                className={selectClass}
                disabled={pending}
                value={draft.responders}
                onChange={(e) =>
                  update({ responders: e.target.value as ResponderRule })
                }
              >
                <option value="members">Channel members</option>
                <option value="role:admin">Community admins</option>
                <option value="role:owner">Community owners</option>
              </select>
            </div>
            <div className="space-y-1">
              <label htmlFor={fid("expires")} className="text-sm">
                Deadline
              </label>
              <select
                id={fid("expires")}
                className={selectClass}
                disabled={pending}
                value={draft.expiresIn}
                onChange={(e) => update({ expiresIn: Number(e.target.value) })}
              >
                {EXPIRY_PRESETS.map((preset) => (
                  <option key={preset.seconds} value={preset.seconds}>
                    {preset.label}
                  </option>
                ))}
              </select>
            </div>
          </div>

          {error ? (
            <p role="alert" className="text-sm text-red-500">
              {error}
            </p>
          ) : null}

          <DialogFooter>
            <Button
              disabled={pending}
              onClick={() => onOpenChange(false)}
              type="button"
              variant="ghost"
            >
              Cancel
            </Button>
            <Button disabled={pending} type="submit">
              {pending
                ? "Sending…"
                : draft.type === "poll"
                  ? "Start poll"
                  : "Ask"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
