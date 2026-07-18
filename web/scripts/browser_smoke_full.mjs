/**
 * Full-opening smoke test of the unified UI: enables the evaluation panel and
 * drives the whole placement phase through the move list (which applies
 * captures atomically), then plays one movement-phase move and checks the
 * panel. Works against scripts/serve.mjs (neural engine) or `ninemm serve`.
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

await page.locator("#evalCheckbox").check();
await page.waitForTimeout(300);

async function currentState() {
  const status = (await page.textContent("#status")) ?? "";
  const stones = await page.locator("svg circle.stone").count();
  return { status, stones };
}

// Drive the opening: whenever it's the human's (White's) turn, apply the
// first move-list entry; the engine answers on its own.
let guard = 0;
while (guard++ < 60) {
  const { status } = await currentState();
  if (!status.includes("Placement phase")) break;
  if (status.includes("White to move")) {
    const rows = page.locator("#moveList li");
    if ((await rows.count()) > 0) await rows.first().click();
  }
  await page.waitForTimeout(700); // engine reply + re-analysis
}

const final = await currentState();
console.log("status after opening:", final.status);
console.log("stones on board after opening:", final.stones);

const reachedMovementPhase = final.status.includes("Movement phase");
console.log("reached movement phase:", reachedMovementPhase);

let playedMovementMove = false;
let sawEvalRows = false;
if (reachedMovementPhase) {
  // Wait for White's turn, then play the top-ranked move from the list.
  for (let i = 0; i < 20; i++) {
    const status = (await page.textContent("#status")) ?? "";
    if (status.includes("White to move")) break;
    await page.waitForTimeout(500);
  }
  sawEvalRows = (await page.locator("#moveList li").count()) > 0;
  const before = (await page.textContent("#status")) ?? "";
  const rows = page.locator("#moveList li");
  if ((await rows.count()) > 0) {
    await rows.first().click();
    await page.waitForTimeout(1500);
    const after = (await page.textContent("#status")) ?? "";
    playedMovementMove = after !== before || (await page.locator("svg circle.stone").count()) !== final.stones;
    console.log("status after one movement-phase move:", after);
  }
}

await browser.close();

if (consoleErrors.length > 0) {
  console.error("CONSOLE ERRORS:", consoleErrors);
  process.exit(1);
}
if (final.stones < 12 || final.stones > 18) {
  console.error(`implausible stone count after the opening: ${final.stones}`);
  process.exit(1);
}
if (!reachedMovementPhase) {
  console.error("did not reach movement phase after the opening");
  process.exit(1);
}
if (!sawEvalRows) {
  console.error("evaluation panel showed no move rows in movement phase");
  process.exit(1);
}
if (!playedMovementMove) {
  console.error("clicking a move-list row did not apply a movement-phase move");
  process.exit(1);
}
console.log("full smoke test passed");
