import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..", "..");
const source = readFileSync(join(here, "NativeNotificationBridgeView.svelte"), "utf8");

const scratch = join(root, "tmp");
mkdirSync(scratch, { recursive: true });
const generatedDir = mkdtempSync(join(scratch, "vitest-native-bridge-"));
const receiptCopyPath = join(generatedDir, "NativeBridgeReceipt.svelte");
const errorCopyPath = join(generatedDir, "NativeBridgeError.svelte");

const STATE_LINE =
  "let current = $state<NotificationDelivery | null>(null), notice = $state<Attention | null>(null), task = $state<Task | null>(null);";
const ERROR_LINE = 'let error = $state(""), busy = $state(false);';

function rewriteImports(svelte: string): string {
  return svelte
    .replace(/from "(\.\.?\/[^"]+)"/g, (_m, spec: string) => `from "${join(here, spec).replaceAll("\\", "/")}"`)
    .replace(/from "\.\/Logo\.svelte"/g, `from "${join(here, "Logo.svelte").replaceAll("\\", "/")}"`);
}

beforeAll(() => {
  expect(source).toContain(STATE_LINE);
  expect(source).toContain(ERROR_LINE);

  const withReceipt = rewriteImports(
    source.replace(
      STATE_LINE,
      'let current = $state<NotificationDelivery | null>({ id: "n1", task_id: "t1", native_id: "gitpulse.x.n1", revision: 1, state: "submitted", activated_at: null } as unknown as NotificationDelivery), notice = $state<Attention | null>({ id: "n1", task_id: "t1", title: "Coding agent exited", target_status: "task_deleted" } as unknown as Attention), task = $state<Task | null>(null);',
    ),
  );
  writeFileSync(receiptCopyPath, withReceipt);

  const withError = rewriteImports(
    source.replace(ERROR_LINE, 'let error = $state("Notification activation failed"), busy = $state(false);'),
  );
  writeFileSync(errorCopyPath, withError);
});

afterAll(() => {
  rmSync(generatedDir, { recursive: true, force: true });
});

describe("NativeNotificationBridgeView", () => {
  it("compiles cleanly and wraps <Logo in aria-hidden in both receipt and error cards", () => {
    const { warnings } = compile(source, { generate: "client", filename: "NativeNotificationBridgeView.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
    expect(source).toMatch(/import Logo from "\.\/Logo\.svelte";/);
    expect(source.match(/aria-hidden="true"[^>]*><Logo\b/g)).toHaveLength(2);
  });

  it("renders .receipt with the current GitPulse badge Logo and Close notification button", async () => {
    const component = (await import(/* @vite-ignore */ receiptCopyPath)).default;
    const { body } = render(component);
    expect(body).toMatch(/class="receipt\b/);
    expect(body).toContain("Coding agent exited");
    expect(body).toContain("Close notification");
    expect(body).toContain("M24.04 9.25A10.5 10.5 0 1 0 26.5 16H22.6L20.2 21L16.8 9.9L14.4 16H11.8");
    expect(body).toMatch(/id="gp-[a-z0-9]+-badge"/);
  });

  it("renders .activation-error with the current GitPulse badge Logo and Dismiss button", async () => {
    const component = (await import(/* @vite-ignore */ errorCopyPath)).default;
    const { body } = render(component);
    expect(body).toMatch(/class="activation-error gp-card shadow-float\b/);
    expect(body).toContain("Notification activation failed");
    expect(body).toContain("Dismiss");
    expect(body).toContain("M24.04 9.25A10.5 10.5 0 1 0 26.5 16H22.6L20.2 21L16.8 9.9L14.4 16H11.8");
    expect(body).toMatch(/id="gp-[a-z0-9]+-badge"/);
  });
});
