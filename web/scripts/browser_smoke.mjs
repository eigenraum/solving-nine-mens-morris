/**
 * Quick smoke test of the unified UI (ui/index.html) against whatever server
 * is on the given port (scripts/serve.mjs for the neural-only setup, or
 * `ninemm serve` for the full thing): loads the page, waits for an engine
 * backend to come up, plays one placement and checks the engine replies.
 */
import { chromium } from "playwright";

const port = process.argv[2] ?? 8834;
const browser = await chromium.launch({
  executablePath: "/opt/pw-browsers/chromium",
  args: ["--no-sandbox"],
});
const page = await browser.newPage();

const consoleErrors = [];
page.on("console", (msg) => {
  // Probing for absent backends 404s by design; only real errors count.
  if (msg.type() === "error" && !msg.text().includes("Failed to load resource")) {
    consoleErrors.push(msg.text());
  }
});
page.on("pageerror", (err) => consoleErrors.push(String(err)));

await page.goto(`http://localhost:${port}/`);
await page.waitForFunction(
  () => document.getElementById("status")?.textContent?.includes("Placement phase"),
  { timeout: 15000 }
);
console.log("status after load:", await page.textContent("#status"));
console.log("engine note:", await page.textContent("#engineNote"));

// Human (white) places a stone at the first point.
await page.locator("svg circle.hit").first().click();
await page.waitForTimeout(1200); // let the engine (black) respond

console.log("status after 1 placement + engine reply:", await page.textContent("#status"));

const stoneCount = await page.locator("svg circle.stone").count();
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
