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

async function currentState() {
  const status = await page.textContent("#status");
  const stones = await page.locator("svg circle.stone-w, svg circle.stone-b").count();
  return { status: status ?? "", stones };
}

// Drive the whole opening: click empty points for human placements; if a
// mill closes (status shows "Mill!"), try each stone circle in turn until
// one resolves the capture (only the legal capture targets do anything).
let guard = 0;
while (guard++ < 80) {
  const { status, stones } = await currentState();
  if (status.includes("to move")) break; // reached movement phase
  if (stones >= 18 && !status.includes("Mill")) break;

  if (status.includes("Mill!")) {
    const stoneEls = await page.locator("svg circle.stone-w, svg circle.stone-b").all();
    for (const el of stoneEls) {
      const before = await page.textContent("#status");
      await el.click();
      await page.waitForTimeout(50);
      const after = await page.textContent("#status");
      if (after !== before) break;
    }
  } else {
    const points = await page.locator("svg circle.point").all();
    for (const pt of points) {
      const before = await page.textContent("#status");
      await pt.click();
      await page.waitForTimeout(50);
      const after = await page.textContent("#status");
      if (after !== before) break;
    }
  }
  await page.waitForTimeout(300); // let the engine respond
}

const final = await currentState();
console.log("status after opening:", final.status);
console.log("stones on board after opening:", final.stones);

const evalText = await page.textContent("#evalList");
console.log("eval panel text (first 300 chars):", evalText?.slice(0, 300));

const reachedMovementPhase = final.status.includes("to move");
console.log("reached movement phase:", reachedMovementPhase);

// If it's the human's turn in movement phase, click one of our stones then
// one of its legal destinations, to exercise the trained-model move path
// and the eval panel's TTA-backed WDL bars.
if (reachedMovementPhase && final.status.startsWith("White")) {
  const before = await page.textContent("#evalList");
  const stoneEls = await page.locator("svg circle.stone-w").all();
  outer: for (const stoneEl of stoneEls) {
    await stoneEl.click();
    await page.waitForTimeout(50);
    const points = await page.locator("svg circle.point").all();
    for (const pt of points) {
      const beforeStatus = await page.textContent("#status");
      await pt.click();
      await page.waitForTimeout(50);
      const afterStatus = await page.textContent("#status");
      if (afterStatus !== beforeStatus) break outer;
    }
  }
  await page.waitForTimeout(500);
  const afterMoveStatus = await page.textContent("#status");
  console.log("status after one movement-phase move:", afterMoveStatus);
}

await browser.close();

if (consoleErrors.length > 0) {
  console.error("CONSOLE ERRORS:", consoleErrors);
  process.exit(1);
}
if (final.stones !== 18) {
  console.error(`expected 18 stones after full opening, got ${final.stones}`);
  process.exit(1);
}
if (!reachedMovementPhase) {
  console.error("did not reach movement phase after the opening");
  process.exit(1);
}
console.log("full smoke test passed");
