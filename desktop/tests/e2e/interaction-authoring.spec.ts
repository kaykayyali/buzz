import { expect, test, type Page } from "@playwright/test";
import { installMockBridge } from "../helpers/bridge";
import { waitForAnimations } from "../helpers/animations";

const RELAY = "a".repeat(64);
// Mock #engineering (desktop/src/testing/e2eBridge.ts): every prompt must be
// scoped to the channel the dialog was opened in.
const ENGINEERING = "1c7e1c02-87bb-5e88-b2da-5a7a9432d0c9";

async function openEngineering(
  page: Page,
  enabled = true,
  rejectEventKinds: number[] = [],
) {
  await installMockBridge(
    page,
    { relaySelf: RELAY, rejectEventKinds },
    { seedPreviewFeatures: enabled },
  );
  await page.goto("/");
  await page.getByTestId("channel-engineering").click();
  await expect(page.getByTestId("message-composer-toolbar")).toBeVisible();
}

/**
 * Prompts the mock relay accepted over the WebSocket, paired with its `OK`.
 * Signing alone proves nothing reached the relay, so success assertions read
 * the published events and their acknowledgements.
 */
const publishedPrompts = (page: Page) =>
  page.evaluate(() => {
    const oks = window.__BUZZ_E2E_RELAY_OKS__ ?? [];
    return (window.__BUZZ_E2E_PUBLISHED_EVENTS__ ?? [])
      .filter((e) => e.kind === 40010)
      .map((e) => ({ ...e, ok: oks.find((ok) => ok.id === e.id) ?? null }));
  });

async function expectOneAcceptedPrompt(page: Page) {
  await expect.poll(() => publishedPrompts(page)).toHaveLength(1);
  const [prompt] = await publishedPrompts(page);
  expect(prompt.ok).toEqual({ id: prompt.id, accepted: true, message: "" });
  expect(prompt.tags).toContainEqual(["h", ENGINEERING]);
  return prompt;
}

test("the composer action is hidden until the experiment is enabled", async ({
  page,
}) => {
  await openEngineering(page, false);
  await expect(page.getByTestId("ask-interaction")).toHaveCount(0);
});

test("a buttons request is validated locally, then signed and published", async ({
  page,
}) => {
  await openEngineering(page);
  await page.getByTestId("ask-interaction").click();
  const dialog = page.getByTestId("ask-interaction-dialog");
  await expect(dialog).toBeVisible();
  // Empty question: a specific message, nothing signed.
  await dialog.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("Write the question");
  expect(await publishedPrompts(page)).toHaveLength(0);

  await dialog.getByLabel("Question").fill("Render **The Door**?");
  await dialog.getByRole("button", { name: "Add option" }).click();
  await dialog.getByLabel("Option 3 label").fill("Send back");
  await expect(dialog.getByLabel("Option 3 ID")).toHaveValue("send-back");
  await dialog.getByLabel("Option 3 style").selectOption("default");
  await dialog.getByRole("button", { name: "Remove option 2" }).click();
  await dialog.getByLabel("Closes").selectOption("quorum:2");
  await dialog.getByLabel("Who can answer").selectOption("role:admin");
  await dialog.getByLabel("Deadline").selectOption("3 days");
  await waitForAnimations(page);
  await dialog.screenshot({ path: "test-results/interaction-authoring.png" });
  await dialog.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(dialog).toBeHidden();

  const prompt = await expectOneAcceptedPrompt(page);
  expect(prompt.tags).toContainEqual(["itype", "buttons"]);
  expect(prompt.tags).toContainEqual(["closes", "quorum:2"]);
  expect(prompt.tags).toContainEqual(["responders", "role:admin"]);
  expect(prompt.tags).toContainEqual(["visibility", "public"]);
  expect(prompt.tags).toContainEqual(["opt", "approve", "Approve", "primary"]);
  expect(prompt.tags).toContainEqual(["opt", "send-back", "Send back"]);
  expect(prompt.tags.some((t) => t[0] === "opt" && t[1] === "deny")).toBe(
    false,
  );
  const deadline = Number(prompt.tags.find((t) => t[0] === "deadline")?.[1]);
  const now = Math.floor(Date.now() / 1000);
  expect(deadline).toBeGreaterThan(now + 3 * 86_400 - 120);
  expect(deadline).toBeLessThanOrEqual(now + 3 * 86_400);
  expect(prompt.tags.some((t) => t[0] === "expiration")).toBe(false);
});

test("a poll carries bounded multi-select and keeps entered options on a validation error", async ({
  page,
}) => {
  await openEngineering(page);
  await page.getByTestId("ask-interaction").click();
  const dialog = page.getByTestId("ask-interaction-dialog");
  await dialog.getByRole("radio", { name: "Poll" }).check();
  await dialog.getByLabel("Question").fill("Which thumbnails should we test?");
  await dialog.getByLabel("Option 1 label").fill("The door");
  await dialog.getByLabel("Option 2 label").fill("The key");
  await dialog.getByRole("button", { name: "Add option" }).click();
  await dialog.getByLabel("Option 3 label").fill("The hallway");
  await dialog.getByLabel("at most").fill("5");
  const start = dialog.getByRole("button", { name: "Start poll" });
  await start.click();
  await expect(dialog.getByRole("alert")).toContainText(
    "between the minimum and the option count",
  );
  // The draft survives the failed attempt.
  await expect(dialog.getByLabel("Option 3 label")).toHaveValue("The hallway");
  await dialog.getByLabel("at most").fill("2");
  await start.click();
  await expect(dialog).toBeHidden();
  const poll = await expectOneAcceptedPrompt(page);
  expect(poll.tags).toContainEqual(["itype", "poll"]);
  expect(poll.tags).toContainEqual(["closes", "expiry"]);
  expect(poll.tags).toContainEqual(["min", "1"]);
  expect(poll.tags).toContainEqual(["max", "2"]);
  expect(poll.tags.filter((t) => t[0] === "opt")).toEqual([
    ["opt", "the-door", "The door"],
    ["opt", "the-key", "The key"],
    ["opt", "the-hallway", "The hallway"],
  ]);
});

test("a form publishes typed fields with select choices", async ({ page }) => {
  await openEngineering(page);
  await page.getByTestId("ask-interaction").click();
  const dialog = page.getByTestId("ask-interaction-dialog");
  await dialog.getByRole("radio", { name: "Form" }).check();
  await dialog.getByLabel("Question").fill("Episode details");
  await dialog.getByLabel("Field 1 label").fill("Episode title");
  await dialog.getByRole("button", { name: "Add field" }).click();
  await dialog.getByLabel("Field 2 label").fill("Length");
  await dialog.getByLabel("Field 2 type").selectOption("select");
  await dialog.getByLabel("Field 2 choices").fill("60s, 6min");
  await dialog.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(dialog).toBeHidden();
  const form = await expectOneAcceptedPrompt(page);
  expect(form.tags).toContainEqual(["itype", "form"]);
  expect(form.tags).toContainEqual([
    "field",
    "episode-title",
    "Episode title",
    "text",
    "required",
  ]);
  expect(form.tags).toContainEqual([
    "field",
    "length",
    "Length",
    "select",
    "required",
  ]);
  expect(form.tags).toContainEqual(["optsel", "length", "60s"]);
  expect(form.tags).toContainEqual(["optsel", "length", "6min"]);
  expect(form.tags.some((t) => t[0] === "opt" || t[0] === "min")).toBe(false);
});

test("a relay that refuses the prompt leaves the dialog open with its reason", async ({
  page,
}) => {
  await openEngineering(page, true, [40010]);
  await page.getByTestId("ask-interaction").click();
  const dialog = page.getByTestId("ask-interaction-dialog");
  await dialog.getByLabel("Question").fill("Ship it?");
  await dialog.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("restricted");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByLabel("Question")).toHaveValue("Ship it?");
  await expect.poll(() => publishedPrompts(page)).toHaveLength(1);
  const [refused] = await publishedPrompts(page);
  expect(refused.ok?.accepted).toBe(false);
  expect(refused.tags).toContainEqual(["h", ENGINEERING]);
});
