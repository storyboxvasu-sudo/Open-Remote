/**
 * Comprehensive Automated Test Suite for OpenRemote Signaling Server
 */

const { spawn } = require("child_process");
const http = require("http");
const WebSocket = require("ws");

const PORT = 8799;

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function runTests() {
  console.log("Starting signaling server test on port", PORT);

  const serverProc = spawn("node", ["server.js"], {
    cwd: __dirname,
    env: { ...process.env, PORT: String(PORT) },
    stdio: "inherit",
  });

  await sleep(1000);

  let passed = 0;
  let failed = 0;

  function assert(condition, testName) {
    if (condition) {
      console.log(`[PASS] ${testName}`);
      passed++;
    } else {
      console.error(`[FAIL] ${testName}`);
      failed++;
    }
  }

  try {
    // 1. Health check HTTP test
    const health = await new Promise((resolve, reject) => {
      http.get(`http://127.0.0.1:${PORT}/health`, (res) => {
        let data = "";
        res.on("data", (chunk) => (data += chunk));
        res.on("end", () => resolve(JSON.parse(data)));
      }).on("error", reject);
    });

    assert(health.status === "ok", "Health endpoint responds ok");

    // 2. Peer A and Peer B connection
    const wsA = new WebSocket(`ws://127.0.0.1:${PORT}`);
    const wsB = new WebSocket(`ws://127.0.0.1:${PORT}`);

    await Promise.all([
      new Promise((res) => wsA.on("open", res)),
      new Promise((res) => wsB.on("open", res)),
    ]);
    assert(true, "WebSockets connected to server");

    // 3. Register Peer A
    const regPromiseA = new Promise((resolve) => {
      wsA.on("message", (raw) => {
        const msg = JSON.parse(raw.toString());
        if (msg.action === "registered") resolve(msg.payload);
      });
    });
    wsA.send(JSON.stringify({ action: "register", payload: { peer_id: "901-435-944" } }));
    const regA = await regPromiseA;
    assert(regA.success === true, "Peer A registered successfully");

    // 4. Dialing Offline Peer: Peer A sends offer to unregistered target "999-888-777"
    const offlinePromise = new Promise((resolve) => {
      wsA.on("message", (raw) => {
        const msg = JSON.parse(raw.toString());
        if (msg.action === "peer_not_found") resolve(msg.payload);
      });
    });
    wsA.send(JSON.stringify({
      action: "offer",
      payload: { target: "999-888-777", sdp: "dummy_sdp_offer" },
    }));
    const offlineRes = await offlinePromise;
    assert(
      offlineRes.reason === "Partner ID is offline or not registered.",
      "Offline target correctly returns 'Partner ID is offline or not registered.'"
    );

    // 5. Register Peer B
    const regPromiseB = new Promise((resolve) => {
      wsB.on("message", (raw) => {
        const msg = JSON.parse(raw.toString());
        if (msg.action === "registered") resolve(msg.payload);
      });
    });
    wsB.send(JSON.stringify({ action: "register", payload: { peer_id: "925-447-871" } }));
    const regB = await regPromiseB;
    assert(regB.success === true, "Peer B registered successfully");

    // 6. Offer from Peer A to Peer B
    const offerPromiseB = new Promise((resolve) => {
      wsB.on("message", (raw) => {
        const msg = JSON.parse(raw.toString());
        if (msg.action === "offer") resolve(msg.payload);
      });
    });
    wsA.send(JSON.stringify({
      action: "offer",
      payload: { target: "925-447-871", sdp: "sdp_offer_from_a", from: "901-435-944" },
    }));
    const receivedOffer = await offerPromiseB;
    assert(receivedOffer.sdp === "sdp_offer_from_a", "Offer routed accurately to Peer B");

    // 7. Answer from Peer B to Peer A
    const answerPromiseA = new Promise((resolve) => {
      wsA.on("message", (raw) => {
        const msg = JSON.parse(raw.toString());
        if (msg.action === "answer") resolve(msg.payload);
      });
    });
    wsB.send(JSON.stringify({
      action: "answer",
      payload: { target: "901-435-944", sdp: "sdp_answer_from_b", from: "925-447-871" },
    }));
    const receivedAnswer = await answerPromiseA;
    assert(receivedAnswer.sdp === "sdp_answer_from_b", "Answer routed accurately to Peer A");

    // 8. ICE Candidate from Peer A to Peer B
    const candidatePromiseB = new Promise((resolve) => {
      wsB.on("message", (raw) => {
        const msg = JSON.parse(raw.toString());
        if (msg.action === "candidate") resolve(msg.payload);
      });
    });
    wsA.send(JSON.stringify({
      action: "candidate",
      payload: { target: "925-447-871", candidate: { candidate: "candidate:1 1 UDP..." } },
    }));
    const receivedCandidate = await candidatePromiseB;
    assert(
      receivedCandidate.candidate.candidate === "candidate:1 1 UDP...",
      "ICE candidate routed accurately to Peer B"
    );

    // 9. Ping / Pong keepalive
    const pongPromise = new Promise((resolve) => {
      wsA.on("message", (raw) => {
        const msg = JSON.parse(raw.toString());
        if (msg.action === "pong") resolve(msg.payload);
      });
    });
    wsA.send(JSON.stringify({ action: "ping" }));
    const pong = await pongPromise;
    assert(typeof pong.timestamp === "number", "Ping/Pong keepalive verified");

    wsA.close();
    wsB.close();
  } catch (err) {
    console.error("Test error:", err);
    failed++;
  } finally {
    serverProc.kill();
  }

  console.log(`\nResults: ${passed} passed, ${failed} failed.`);
  process.exit(failed > 0 ? 1 : 0);
}

runTests();
