// Verify the standalone report in installed Chrome using CDP. No npm packages.
// CHROME=/path/to/chrome node docs/review/verify.mjs
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, existsSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));
const output = mkdtempSync(join(tmpdir(),'vr-fire-report-check-'));
const profile = join(output,'profile');
const chrome = spawn(process.env.CHROME || 'google-chrome', [
  '--headless', '--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage',
  '--no-first-run', '--no-default-browser-check', '--disable-background-networking',
  '--disable-component-update', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'
], {stdio:['ignore','ignore','pipe']});
let ws;
try {
  const endpoint = await new Promise((resolve,reject)=>{
    let stderr='';
    const timer=setTimeout(()=>reject(new Error('Chrome did not expose CDP: '+stderr.slice(-1500))),15000);
    chrome.once('error',e=>{clearTimeout(timer);reject(e);});
    chrome.once('exit',code=>{clearTimeout(timer);reject(new Error(`Chrome exited ${code}: ${stderr.slice(-1500)}`));});
    chrome.stderr.on('data',data=>{stderr+=data;const match=stderr.match(/DevTools listening on (ws:\/\/\S+)/);if(match){clearTimeout(timer);resolve(match[1]);}});
  });
  ws = new WebSocket(endpoint);
  await new Promise((resolve,reject)=>{ws.addEventListener('open',resolve,{once:true});ws.addEventListener('error',reject,{once:true});});
  let next=0;
  const pending=new Map(), errors=[], network=[];
  ws.addEventListener('message',({data})=>{
    const m=JSON.parse(data);
    if(m.id){const p=pending.get(m.id);if(p){pending.delete(m.id);clearTimeout(p.timer);m.error?p.reject(new Error(JSON.stringify(m.error))):p.resolve(m.result);}}
    if(m.method==='Runtime.exceptionThrown')errors.push(m.params.exceptionDetails);
    if(m.method==='Network.requestWillBeSent'&&/^https?:/.test(m.params.request.url))network.push(m.params.request.url);
  });
  function call(method,params={},sessionId){return new Promise((resolve,reject)=>{const id=++next;const timer=setTimeout(()=>{pending.delete(id);reject(new Error('CDP timeout: '+method));},10000);pending.set(id,{resolve,reject,timer});ws.send(JSON.stringify({id,method,params,...(sessionId?{sessionId}:{})}));});}
  const {targetId}=await call('Target.createTarget',{url:'about:blank'});
  const {sessionId}=await call('Target.attachToTarget',{targetId,flatten:true});
  const page=(method,params={})=>call(method,params,sessionId);
  const evaluate=async expression=>{const r=await page('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});assert(!r.exceptionDetails,JSON.stringify(r.exceptionDetails));return r.result.value;};
  await page('Page.enable');await page('Runtime.enable');await page('Network.enable');
  await page('Network.emulateNetworkConditions',{offline:true,latency:0,downloadThroughput:0,uploadThroughput:0});
  await page('Emulation.setDeviceMetricsOverride',{width:1440,height:1000,deviceScaleFactor:1,mobile:false});
  await page('Page.navigate',{url:pathToFileURL(join(here,'index.html')).href});
  for(let i=0;i<30;i++){if(await evaluate('document.documentElement.dataset.reportReady === "true"'))break;await new Promise(r=>setTimeout(r,100));}
  assert(await evaluate('document.documentElement.dataset.reportReady === "true"'),'Report script did not initialize');
  assert.equal(await evaluate('document.querySelectorAll(".finding").length'),9);
  assert.equal(await evaluate('document.querySelectorAll(".diagram-inner svg").length'),3);
  assert.equal(await evaluate('document.documentElement.scrollWidth > innerWidth'),false,'Desktop horizontal overflow');
  const ids=await evaluate('[...document.querySelectorAll("[id]")].map(e=>e.id)');
  assert.equal(new Set(ids).size,ids.length,'Duplicate HTML/SVG ids');
  const links=await evaluate('[...document.querySelectorAll("a[href]")].map(a=>a.getAttribute("href"))');
  for(const link of links){if(link.startsWith('#'))assert(ids.includes(link.slice(1)),`Missing anchor ${link}`);else if(!/^[a-z]+:/i.test(link))assert(existsSync(resolve(here,link.split('#')[0])),`Missing linked file ${link}`);}
  const visible=()=>evaluate('[...document.querySelectorAll(".finding")].filter(e=>!e.hidden).map(e=>e.id)');
  await evaluate('document.querySelector("#search").value="quantization";document.querySelector("#search").dispatchEvent(new Event("input"))');
  assert.deepEqual(await visible(),['F03','F06']);
  await evaluate('document.querySelector("#search").value="";document.querySelector("#priority").value="P1";document.querySelector("#priority").dispatchEvent(new Event("input"))');
  assert.deepEqual(await visible(),['F01']);
  await evaluate('document.querySelector("#priority").value="";document.querySelector("#area").value="Pipeline";document.querySelector("#area").dispatchEvent(new Event("input"))');
  assert.deepEqual(await visible(),['F03','F07','F08']);
  await evaluate('document.querySelector("#search").value="no-such-finding-9837";document.querySelector("#search").dispatchEvent(new Event("input"))');
  assert.equal(await evaluate('document.querySelector("#no-results").hidden'),false);
  await evaluate('window.dispatchEvent(new Event("beforeprint"))');
  assert.equal((await visible()).length,9);
  assert.equal(await evaluate('document.querySelectorAll(".finding-details[open]").length'),9);
  await evaluate('window.dispatchEvent(new Event("afterprint"))');
  assert.equal((await visible()).length,0,'Print did not restore filters');
  await evaluate('location.hash="F05";showHash()');
  assert.equal((await visible()).length,9);
  assert.equal(await evaluate('document.querySelector("#F05 details").open'),true);
  await evaluate('document.querySelector("#expand").click()');
  assert.equal(await evaluate('document.querySelectorAll(".finding-details[open]").length'),9);
  await evaluate('document.querySelector("#expand").click()');
  await evaluate('document.querySelector("[data-zoom=in]").click()');
  assert.equal(await evaluate('document.querySelector(".diagram-inner").dataset.scale'),'1.5');
  await evaluate('document.querySelector("[data-zoom=reset]").click();history.replaceState(null,"",location.pathname);window.scrollTo(0,0)');
  await new Promise(r=>setTimeout(r,300));
  const shot=async name=>{const r=await page('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});writeFileSync(join(output,name+'.png'),Buffer.from(r.data,'base64'));};
  await shot('desktop');
  await evaluate('document.querySelector("#erd-terrain").scrollIntoView({behavior:"instant"})');
  await shot('erd-terrain');
  await evaluate('document.querySelector("#erd-streaming").scrollIntoView({behavior:"instant"})');
  await shot('erd-streaming');
  await evaluate('document.querySelector("#erd-services").scrollIntoView({behavior:"instant"})');
  await shot('erd-services');
  await page('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:true});
  await evaluate('window.scrollTo({top:0,behavior:"instant"})');
  assert.equal(await evaluate('document.documentElement.scrollWidth > innerWidth'),false,'Mobile horizontal page overflow');
  await shot('mobile');
  assert.equal(errors.length,0,JSON.stringify(errors));
  assert.equal(network.length,0,`External requests: ${network.join(', ')}`);
  // Check embedded code parses even when the HTML is opened without repository assets.
  const html=readFileSync(join(here,'index.html'),'utf8');
  assert(!/<script[^>]+src=|<link[^>]+href=|<img[^>]+src=/i.test(html),'Report depends on external assets');
  console.log(JSON.stringify({result:'PASS',checks:['offline file URL','9 findings','3 SVG ERDs','unique ids','local links','search','priority/component filters','empty state','hash links','expand/collapse','diagram zoom','print expansion/restoration','desktop/mobile layout','no JS exceptions','zero external requests'],screenshots:output},null,2));
} finally {
  ws?.close();
  chrome.kill('SIGTERM');
}
