import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir } from "node:fs/promises";
import { chromium } from "playwright";
import { chatService } from "../examples/paid-chat/config.js";
import { startPaidChat } from "../examples/paid-chat/server.js";
const app = await startPaidChat({
  port: 4321,
  checkoutPort: 4322,
  workspace: "paid-browser-" + randomUUID(),
  quiet: true,
});
const browser = await chromium.launch({
  headless: true,
  ...(process.env.PLAYWRIGHT_EXECUTABLE_PATH
    ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH }
    : {}),
});
const page = await browser.newPage({
  viewport: { width: 1280, height: 1000 },
  reducedMotion: "reduce",
});
const errors: string[] = [];
page.on("pageerror", (e) => errors.push(e.message));
try {
  await page.goto(app.origin);
  await page.getByRole("button", { name: "使用沙盒示範帳戶" }).click();
  await page.getByText("已連線 · demo-customer").waitFor();
  await page.getByRole("button", { name: "選擇方案" }).first().click();
  await page.waitForURL(app.checkoutOrigin + "/**");
  await page.getByRole("link", { name: "稍後付款，返回應用" }).click();
  await page.getByText("等待可信付款通知，回到此頁不會直接授額。").waitFor();
  await page.getByRole("button", { name: "繼續付款" }).click();
  await page.waitForURL(app.checkoutOrigin + "/**");
  await page.getByRole("button", { name: "模擬付款並送出可信通知" }).click();
  await page.waitForURL(app.origin + "/**");
  await page.getByText("付款已確認，當期 credits 已入帳。").waitFor();
  const input = page.getByPlaceholder("輸入你的想法…");
  await input.fill("Hello from browser");
  await input.press("Enter");
  await page.getByText("最近一次 · 已結算").waitFor();
  assert.ok((await page.locator(".sc-thread").innerText()).includes("Hello"));
  await page.getByRole("button", { name: "影片", exact: true }).click();
  await page.getByLabel("任務數量").fill("5");
  await input.fill("建立五秒影片");
  await input.press("Enter");
  await page.getByText("影片 5 秒").waitFor();
  await page.getByText("0.25", { exact: false }).first().waitFor();
  const beforeRetry = await (
    await page.request.get(app.origin + "/api/billing/me")
  ).json();
  page.once("dialog", (dialog) => dialog.dismiss());
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  const afterRetry = await (
    await page.request.get(app.origin + "/api/billing/me")
  ).json();
  assert.equal(
    afterRetry.usage.length,
    beforeRetry.usage.length,
    "canceling a new billable generation never dispatches",
  );
  await mkdir("/tmp/superrouter-paid-chat", { recursive: true });
  await page.screenshot({
    path: "/tmp/superrouter-paid-chat/desktop.png",
    fullPage: true,
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({
    path: "/tmp/superrouter-paid-chat/mobile.png",
    fullPage: true,
  });
  assert.ok(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
    "mobile has no horizontal overflow",
  );
  await app.store.publish(app.host, {
    ...chatService,
    version: 2,
    price: { ...chatService.price, input: "4000", output: "6000" },
  });
  await page.getByRole("button", { name: "對話", exact: true }).click();
  const quotedCalls = app.mock.calls;
  await input.fill("價格更新前的畫面");
  await input.press("Enter");
  await page
    .getByText("費率已更新，本次未扣款。請查看新版費率，再次送出。")
    .waitFor();
  await page
    .getByText("輸入 1 credit＝250000 tokens", { exact: false })
    .waitFor();
  assert.equal(
    app.mock.calls,
    quotedCalls,
    "stale rate preview never dispatches",
  );
  await input.fill("已檢視新費率，再次送出");
  await input.press("Enter");
  await page.getByText("最近一次 · 已結算").waitFor();
  assert.equal(app.mock.calls, quotedCalls + 1);
  await page.getByRole("button", { name: "取消下期續用" }).click();
  await page.getByText("已安排取消，當期權益保留至到期。").waitFor();
  await page.getByRole("button", { name: "登出", exact: true }).click();
  await page.getByRole("button", { name: "使用沙盒示範帳戶" }).waitFor();
  assert.deepEqual(errors, []);
  console.log(
    "Paid-chat browser journey passed; desktop/mobile screenshots: /tmp/superrouter-paid-chat",
  );
} finally {
  await browser.close();
  await app.close();
}
