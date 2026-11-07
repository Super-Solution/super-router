/** Local protocol fixtures only; never calls a live payment gateway. */
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium } from "playwright";
import { startPaidChat } from "../examples/paid-chat/server.js";
import type { BillingSnapshot } from "../packages/bindings/react/src/index.js";

const output = resolve(
  process.env.PAID_GATEWAYS_SCREENSHOTS ?? "/tmp/superrouter-paid-gateways",
);
await mkdir(output, { recursive: true });
const browser = await chromium.launch({
  headless: true,
  ...(process.env.PLAYWRIGHT_EXECUTABLE_PATH
    ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH }
    : {}),
});
try {
  for (const provider of ["stripe", "oen", "epay"] as const) {
    const app = await startPaidChat({
      port: 4331,
      checkoutPort: 4332,
      workspace: `paid-browser-${provider}-${randomUUID()}`,
      quiet: true,
      paymentMode: "fixture",
    });
    const context = await browser.newContext({
      viewport: { width: 1280, height: 1000 },
      reducedMotion: "reduce",
    });
    const page = await context.newPage();
    page.setDefaultTimeout(15000);
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    // Fail before sending anything outside localhost: these are protocol fixtures, not live smoke tests.
    await context.route("**/*", async (route) => {
      const url = new URL(route.request().url());
      if (
        !["http:", "https:"].includes(url.protocol) ||
        url.hostname === "127.0.0.1"
      ) {
        await route.continue();
      } else {
        errors.push(`Unexpected external browser request: ${url.origin}`);
        await route.abort("blockedbyclient");
      }
    });
    const snapshot = async (): Promise<BillingSnapshot> => {
      const response = await page.request.get(app.origin + "/api/billing/me");
      assert.equal(response.status(), 200);
      return response.json();
    };
    const available = (value: BillingSnapshot) =>
      value.buckets.reduce(
        (total, bucket) => total + BigInt(bucket.available),
        0n,
      );
    try {
      await page.goto(app.origin);
      await page.getByRole("button", { name: "使用沙盒示範帳戶" }).click();
      await page.getByText("已連線 · demo-customer").waitFor();
      await page.getByLabel("付款通路").selectOption(provider);
      assert.equal(await page.getByLabel("付款通路").inputValue(), provider);
      const before = await snapshot();
      assert.equal(available(before), 0n);
      assert.equal(before.entitlements.length, 0);
      await page
        .getByRole("button", { name: "選擇方案", exact: true })
        .first()
        .click();
      await page.waitForURL(app.checkoutOrigin + "/**");
      assert.equal(new URL(page.url()).searchParams.get("provider"), provider);
      await page.screenshot({
        path: `${output}/${provider}-checkout.png`,
        fullPage: true,
      });
      // Returning from checkout without the signed notification must not grant credits.
      await page.getByRole("link", { name: "稍後付款，返回應用" }).click();
      await page
        .getByText("等待可信付款通知，回到此頁不會直接授額。")
        .waitFor();
      assert.equal(available(await snapshot()), 0n);
      await page.getByRole("button", { name: "繼續付款" }).click();
      await page.waitForURL(app.checkoutOrigin + "/**");
      assert.equal(new URL(page.url()).searchParams.get("provider"), provider);
      await page
        .getByRole("button", { name: "模擬付款並送出可信通知" })
        .click();
      await page.waitForURL(app.origin + "/**");
      await page.getByText("付款已確認，當期 credits 已入帳。").waitFor();
      await page
        .locator(".sr-balance strong")
        .filter({ hasText: /^10\s*credits$/ })
        .waitFor();
      assert.equal(
        await page.getByLabel("付款通路").inputValue(),
        provider,
        "return page retains the purchased gateway and currency",
      );
      const paid = await snapshot();
      assert.equal(available(paid), 10000000000n);
      assert.equal(paid.buckets.length, 1);
      assert.equal(paid.entitlements.length, 1);
      assert.equal(paid.orders.length, 1);
      assert.equal(paid.orders[0]!.state, "succeeded");
      assert.equal(
        paid.orders[0]!.currency,
        provider === "epay" ? "CNY" : "TWD",
      );
      assert.ok(
        (await page.locator(".sr-price").first().innerText()).startsWith(
          paid.orders[0]!.currency,
        ),
        "visible pricing currency matches the paid order",
      );
      assert.equal(paid.subscriptions[0]!.status, "active");
      await page.reload();
      await page.getByText("付款已確認，當期 credits 已入帳。").waitFor();
      assert.equal(await page.getByLabel("付款通路").inputValue(), provider);
      const replay = await snapshot();
      assert.equal(
        available(replay),
        available(paid),
        "return-page replay never grants twice",
      );
      assert.equal(replay.buckets.length, paid.buckets.length);
      const composer = page.getByPlaceholder("輸入你的想法…");
      await composer.fill(`Browser protocol fixture: ${provider}`);
      await composer.press("Enter");
      await page.getByText("最近一次 · 已結算").waitFor();
      const used = await snapshot();
      assert.equal(used.usage.length, 1);
      assert.equal(used.usage[0]!.billing, "settled");
      assert.ok(BigInt(used.usage[0]!.charged) > 0n);
      assert.equal(
        available(used),
        available(paid) - BigInt(used.usage[0]!.charged),
      );
      await page.screenshot({
        path: `${output}/${provider}-desktop.png`,
        fullPage: true,
      });
      await page.setViewportSize({ width: 390, height: 844 });
      await page.screenshot({
        path: `${output}/${provider}-mobile.png`,
        fullPage: true,
      });
      assert.ok(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      );
      assert.deepEqual(
        errors,
        [],
        `${provider}: browser runtime/external request errors`,
      );
      console.log(
        `${provider}: browser checkout → verified fixture payment → 10 credits → inference debit passed`,
      );
    } catch (error) {
      await page
        .screenshot({
          path: `${output}/${provider}-failure.png`,
          fullPage: true,
        })
        .catch(() => {});
      throw error;
    } finally {
      await context.close();
      await app.close();
    }
  }
  console.log(
    `Three local payment gateway browser journeys passed. Screenshots: ${output}`,
  );
} finally {
  await browser.close();
}
