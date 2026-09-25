"""Run the Bevy wasm terrain benchmark in Chrome (WebGPU and WebGL2) and save the results.

Serves bench/ on localhost, opens bench/bevy/web/ with vsync and the frame-rate limit off,
and reads the `BENCH_RESULT {json}` line the app logs. Needs `pip install playwright` and
Google Chrome. Usage: python bench/scripts/run_web.py [--headed]
"""
import asyncio, functools, http.server, json, pathlib, re, sys, threading

from playwright.async_api import async_playwright

BENCH = pathlib.Path(__file__).resolve().parent.parent
PORT = 8811


def serve():
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(BENCH))
    handler.log_message = lambda *a: None
    http.server.ThreadingHTTPServer(("127.0.0.1", PORT), handler).serve_forever()


async def run(p, api, headed):
    browser = await p.chromium.launch(channel="chrome", headless=not headed, args=[
        "--enable-unsafe-webgpu", "--enable-features=Vulkan,WebGPU", "--use-angle=vulkan", "--ignore-gpu-blocklist",
        "--disable-gpu-vsync", "--disable-frame-rate-limit"])
    page = await browser.new_page(viewport={"width": 1920, "height": 1080})
    result = asyncio.get_running_loop().create_future()
    def on_console(m):
        hit = re.search(r"BENCH_RESULT (\{.*\})", m.text)
        if hit and not result.done():
            result.set_result(json.loads(hit.group(1)))
        elif "bench api" in m.text or "ERROR" in m.text or "panicked" in m.text:
            print(f"  [{api}] {m.text[:200]}")
    page.on("console", on_console)
    await page.goto(f"http://127.0.0.1:{PORT}/bevy/web/?api={api}")
    try:
        r = await asyncio.wait_for(result, 240)
    finally:
        await page.screenshot(path=str(BENCH / "results" / f"bevy_wasm_{api}.png"))
        await browser.close()
    r["browser"] = f"Chrome ({'headed' if headed else 'headless'})"
    return r


async def main():
    headed = "--headed" in sys.argv
    threading.Thread(target=serve, daemon=True).start()
    (BENCH / "results").mkdir(exist_ok=True)
    async with async_playwright() as p:
        for api in ["webgpu", "webgl2"]:
            r = await run(p, api, headed)
            (BENCH / "results" / f"bevy_wasm_{api}.json").write_text(json.dumps(r, indent=2))
            f = r["frame_ms"]
            print(f"{api}: {r['graphics_api']} {r['fps_mean']} fps, p50 {f['p50']:.2f} p99 {f['p99']:.2f} ms, {r['frames']} frames")


asyncio.run(main())
