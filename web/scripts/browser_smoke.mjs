import { chromium } from "playwright";

const port = process.argv[2] ?? 8834;
const browser = await chromium.launch({
  executablePath: "/opt/pw-browsers/chromium",
  args: ["--no-sandbox"],
});
const page = await browser.newPage();

const consoleErrors = [];
page.on("console", (msg) => {
  if (msg.type() === "error") consoleErrors.push(msg.text());
});
page.on("pageerror", (err) => consoleErrors.push(String(err)));

await page.goto(`http://localhost:${port}/public/index.html`);
await page.waitForFunction(
  () => document.getElementById("status")?.textContent?.includes("to place"),
  { timeout: 10000 }
);
console.log("status after load:", await page.textContent("#status"));

// Human (white) places a stone at the first clickable point.
const firstPoint = await page.locator("svg circle.point").first();
await firstPoint.click();
await page.waitForTimeout(600); // let the engine (black) respond

console.log("status after 1 placement + engine reply:", await page.textContent("#status"));

const stoneCount = await page.locator("svg circle.stone-w, svg circle.stone-b").count();
console.log("stones on board after one exchange:", stoneCount);

if (consoleErrors.length > 0) {
  console.error("CONSOLE ERRORS:", consoleErrors);
  await browser.close();
  process.exit(1);
}
if (stoneCount < 2) {
  console.error("expected at least 2 stones on the board after one exchange");
  await browser.close();
  process.exit(1);
}

console.log("browser smoke test passed, no console errors");
await browser.close();
