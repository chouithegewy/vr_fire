// Regenerate the offline report with Node.js and Graphviz (`dot`). No npm packages.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, resolve, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../..');
const readJSON = name => JSON.parse(readFileSync(resolve(here, name), 'utf8'));
const report = readJSON('report.json');
const validation = readJSON('validation.json');
const diagrams = readJSON('diagrams.json');
const esc = s => String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const sourceCache = new Map();
function source(file) {
  if (!sourceCache.has(file)) {
    const committed = execFileSync('git', ['show', `${report.revision}:${file}`], {cwd:root, encoding:'utf8'});
    const current = readFileSync(resolve(root, file), 'utf8');
    if (current !== committed) throw new Error(`Source changed since reviewed revision: ${file}`);
    sourceCache.set(file, committed);
  }
  return sourceCache.get(file);
}
const sourceLink = file => `<a href="../../${esc(file)}">${esc(file)}</a>`;
function evidence(item) {
  const lines = source(item.file).split('\n');
  if (item.start < 1 || item.end >= lines.length || item.end < item.start) throw new Error(`Invalid source range: ${item.file}`);
  return `<div class="evidence"><div class="evidence-label">${sourceLink(item.file)} · lines ${item.start}–${item.end}</div><pre><code>${lines.slice(item.start-1,item.end).map((line,i)=>`<span class="ln">${i+item.start}</span>${esc(line)}`).join('\n')}</code></pre></div>`;
}

mkdirSync(resolve(here,'diagrams'), {recursive:true});
function renderDiagram(d) {
  d.sources.forEach(source);
  const q = JSON.stringify;
  const rows = n => [
    `<TR><TD ALIGN="LEFT" BGCOLOR="#183b45"><FONT COLOR="#ffffff" POINT-SIZE="15"><B>${esc(n.name)}</B></FONT></TD></TR>`,
    `<TR><TD ALIGN="LEFT" BGCOLOR="#e6efed"><FONT COLOR="#325c60" POINT-SIZE="10">${esc(n.kind)}</FONT></TD></TR>`,
    ...n.fields.map(f=>`<TR><TD ALIGN="LEFT">${esc(f)}</TD></TR>`)
  ].join('');
  const dot = [
    `digraph ${d.id} {`,
    'graph [rankdir=LR, bgcolor="transparent", pad="0.35", nodesep="0.45", ranksep="1.0", splines=polyline, outputorder=edgesfirst];',
    'node [shape=plain, fontname="DejaVu Sans", fontsize=11, fontcolor="#18333d"];',
    'edge [fontname="DejaVu Sans", fontsize=10, color="#527476", fontcolor="#335e62", arrowsize=0.65, labeldistance=2.1];',
    ...d.nodes.map(n=>`${n.id} [label=<<TABLE BORDER="1" COLOR="#b4c9c6" CELLBORDER="0" CELLSPACING="0" CELLPADDING="8" BGCOLOR="#ffffff">${rows(n)}</TABLE>>];`),
    ...d.edges.map(e=>`${e.from} -> ${e.to} [label=${q(e.label)}, taillabel=${q(e.a)}, headlabel=${q(e.b)}, style=${e.derived?'dashed':'solid'}];`),
    '}'
  ].join('\n');
  writeFileSync(resolve(here,`diagrams/${d.id}.dot`),dot+'\n');
  let svg = execFileSync('dot',['-Tsvg'],{input:dot,encoding:'utf8',maxBuffer:4*1024*1024});
  svg = svg.slice(svg.indexOf('<svg')).replace(/<!--[^]*?-->/g,'');
  // Graphviz IDs repeat across diagrams; namespace them for valid inline HTML.
  svg = svg.replace(/id="([^"]+)"/g,(_,id)=>`id="${d.id}-${id}"`);
  svg = svg.replace(/<svg\b/,`<svg role="img" aria-labelledby="${d.id}-title ${d.id}-desc"`);
  svg = svg.replace(/(<svg[^>]*>)/,`$1<title id="${d.id}-title">${esc(d.title)}</title><desc id="${d.id}-desc">${esc(d.subtitle+' '+d.notes)}</desc>`);
  writeFileSync(resolve(here,`diagrams/${d.id}.svg`),svg);
  return `<section class="diagram-card" id="erd-${d.id}"><div class="diagram-heading"><div><span class="eyebrow">ERD ${diagrams.indexOf(d)+1} / 3</span><h3>${esc(d.title)}</h3><p>${esc(d.subtitle)}</p></div><div class="zoom-tools" aria-label="Diagram zoom"><button data-zoom="out" aria-label="Zoom out ${esc(d.title)}">−</button><button data-zoom="reset">Fit</button><button data-zoom="in" aria-label="Zoom in ${esc(d.title)}">+</button></div></div><div class="diagram-viewport" tabindex="0" role="region" aria-label="${esc(d.title)} diagram; scroll when zoomed"><div class="diagram-inner">${svg}</div></div><p class="diagram-note">${esc(d.notes)}</p><details class="dictionary"><summary>Entity dictionary and relationships (text alternative)</summary><div class="table-wrap"><table><thead><tr><th>Entity</th><th>Storage / identity / fields</th></tr></thead><tbody>${d.nodes.map(n=>`<tr><td><strong>${esc(n.name)}</strong></td><td>${esc(n.kind)}<br>${n.fields.map(esc).join('<br>')}</td></tr>`).join('')}</tbody></table></div><ul>${d.edges.map(e=>`<li>${esc(d.nodes.find(n=>n.id===e.from).name)} [${esc(e.a)}] → ${esc(d.nodes.find(n=>n.id===e.to).name)} [${esc(e.b)}]: ${esc(e.label)}${e.derived?' (derived)':''}</li>`).join('')}</ul></details><p class="source-list">Source: ${d.sources.map(sourceLink).join(' · ')} · <a href="diagrams/${d.id}.svg">SVG</a> · <a href="diagrams/${d.id}.dot">DOT</a></p></section>`;
}
const renderedDiagrams = diagrams.map(renderDiagram).join('\n');
const p1 = report.findings.filter(f=>f.priority==='P1').length;
const reproduced = report.findings.filter(f=>f.confidence==='Reproduced').length;
const findingHTML = report.findings.map(f=>`<article class="finding" id="${f.id}" data-priority="${f.priority}" data-area="${esc(f.area)}" data-confidence="${esc(f.confidence)}"><div class="finding-meta"><a class="finding-id" href="#${f.id}">${f.id}</a><span class="badge ${f.priority.toLowerCase()}">${f.priority}</span><span>${esc(f.area)}</span><span class="confidence ${f.confidence==='Reproduced'?'confirmed':''}">${esc(f.confidence)}</span></div><h3>${esc(f.title)}</h3><p class="impact">${esc(f.impact)}</p><details class="finding-details"><summary>Read analysis, remedy & source evidence</summary><div class="finding-body"><dl><dt>Trigger</dt><dd>${esc(f.trigger)}</dd><dt>Cause</dt><dd>${esc(f.detail)}</dd><dt>Recommended change</dt><dd>${esc(f.recommendation)}</dd><dt>Acceptance check</dt><dd>${esc(f.acceptance)}</dd>${f.reproduction?`<dt>Observed reproduction</dt><dd class="repro">${esc(f.reproduction)}</dd>`:''}</dl>${f.evidence.map(evidence).join('')}</div></details></article>`).join('\n');
const checksHTML = validation.checks.map(c=>`<tr><td><strong>${esc(c.name)}</strong><br><span class="pass">${esc(c.result)}</span></td><td><code>${esc(c.command)}</code><p>${esc(c.note)}</p></td></tr>`).join('');
const remediations = [
  ['01','Bound relay resources','F01','Put queue limits and write deadlines around slow clients.'],
  ['02','Preserve terrain integrity','F03 · F06 · F07 · F08','Keep validity information, validate inputs, and publish coherent versions.'],
  ['03','Make streaming recoverable','F02 · F04 · F05','Budget the cache and test failure and source-switch transitions.'],
  ['04','Make service accounting durable','F09','Persist request reservations across same-day restarts.']
];

const css = `
:root{color-scheme:light;--ink:#18333d;--muted:#536970;--paper:#f4f5f1;--card:#fff;--line:#dce4df;--accent:#20766d;--nav:#142f38;--orange:#a54f25;--navw:228px}
*{box-sizing:border-box}html{scroll-behavior:smooth;scroll-padding-top:24px}body{margin:0;background:var(--paper);color:var(--ink);font:15px/1.65 system-ui,-apple-system,"Segoe UI",sans-serif}a{color:#176c65;text-underline-offset:3px}button,input,select{font:inherit}button{cursor:pointer}button:focus-visible,a:focus-visible,input:focus-visible,select:focus-visible,summary:focus-visible,[tabindex]:focus-visible{outline:3px solid #e4a345;outline-offset:4px}.skip{position:fixed;left:12px;top:-80px;z-index:10;background:white;padding:10px}.skip:focus{top:10px}
aside{position:fixed;inset:0 auto 0 0;width:var(--navw);background:var(--nav);color:#c2d4d4;padding:34px 25px;display:flex;flex-direction:column}.brand{color:#fff;font-size:25px;font-weight:750;letter-spacing:-1px}.brand span{color:#8ec5a8}.side-caption{font-size:11px;letter-spacing:2px;text-transform:uppercase;color:#afc9cc;margin:4px 0 38px}nav{display:grid;gap:8px}nav a{display:flex;gap:15px;padding:10px 0;color:#dce7e6;text-decoration:none;font-size:13px}nav a:hover{color:#a4d7ba}nav span{font-size:11px;color:#8aacb0;font-variant-numeric:tabular-nums}.side-bottom{margin-top:auto;border-top:1px solid #34505a;padding-top:20px;font-size:12px}.side-bottom code{color:#c0d5c8}.side-bottom a{color:#b7dacd}.dot{display:inline-block;border-radius:50%;width:7px;height:7px;background:#8fc7aa;margin-right:7px}
main{max-width:1600px;margin-left:var(--navw);padding:44px clamp(24px,4.5vw,78px) 60px}.topline{display:flex;justify-content:space-between;align-items:center;gap:16px;margin-bottom:34px}.eyebrow{font-size:11px;letter-spacing:1.8px;text-transform:uppercase;font-weight:750;color:var(--accent)}.quiet-button{background:transparent;border:1px solid #b6c7c1;border-radius:6px;padding:7px 13px;color:var(--ink);font-size:12px}.quiet-button:hover{background:#e8eeea}h1,h2,h3,p{margin-top:0}h1{font-size:clamp(38px,4.1vw,64px);line-height:1.08;letter-spacing:-2.5px;font-weight:730;margin-bottom:22px}h1 em{font-style:normal;color:#27796f}h2{font-size:28px;letter-spacing:-.7px;line-height:1.2;margin-bottom:13px}h3{font-size:20px;line-height:1.4;letter-spacing:-.3px;margin-bottom:10px}.lede{font-size:17px;color:#53666c;max-width:820px;line-height:1.75}.scope{font-size:13px;color:var(--muted);max-width:900px}.scope strong{color:var(--ink)}.hero{border-bottom:1px solid var(--line);padding-bottom:32px;margin-bottom:36px}.metrics{display:grid;grid-template-columns:repeat(4,1fr);gap:12px;margin:30px 0 22px}.metric{padding:19px 20px;background:white;border:1px solid var(--line);border-radius:9px}.metric b{font-size:35px;line-height:1.2;display:block;font-weight:650;letter-spacing:-1px}.metric span{font-size:12px;color:var(--muted)}.metric small{display:block;color:var(--muted);font-size:11px}.metric.alert b{color:var(--orange)}.metric.good b{color:var(--accent)}.callout{background:#e8efea;border-left:3px solid #5a947e;padding:15px 19px;font-size:13px}.section{margin:0 0 48px}.section-intro{color:var(--muted);max-width:860px;font-size:14px}.section-top{display:flex;justify-content:space-between;align-items:center;gap:16px}.pill{font-size:11px;white-space:nowrap;border:1px solid var(--line);padding:4px 9px;border-radius:30px;color:var(--muted)}.architecture{display:grid;grid-template-columns:repeat(3,1fr);gap:14px;margin-top:24px}.arch-card{background:white;border:1px solid var(--line);border-radius:8px;padding:20px}.arch-card h3{font-size:16px}.arch-card p{font-size:13px;margin-bottom:9px;color:var(--muted)}.arch-card code{font-size:11px;color:#266d65}
.filters{display:flex;flex-wrap:wrap;gap:9px;align-items:center;margin:22px 0 12px}.filters input{flex:1 1 240px;min-width:0}.filters input,.filters select{background:#fff;border:1px solid #bccbc4;border-radius:6px;padding:9px 11px;font-size:13px;color:var(--ink)}.result-count{font-size:12px;color:var(--muted);margin:12px 0}.finding{background:var(--card);border:1px solid var(--line);border-radius:9px;margin:12px 0;padding:22px 24px;scroll-margin-top:25px}.finding:target{border-color:#408b76;box-shadow:0 0 0 3px #cee4d8}.finding-meta{display:flex;align-items:center;gap:11px;font-size:11px;color:var(--muted);margin-bottom:12px}.finding-id{font-weight:750;letter-spacing:1px;text-decoration:none;color:#436c6c}.badge{padding:2px 7px;font-weight:750;border-radius:4px}.p1{background:#fff0e5;color:#a2481d}.p2{background:#eef1dc;color:#616925}.confidence{margin-left:auto;font-size:10px;border:1px solid #d4dfda;border-radius:20px;padding:2px 8px}.confidence.confirmed{background:#eaf4ee;color:#207052;border-color:#c9e1d0}.impact{font-size:14px;color:#496168;margin-bottom:15px;max-width:1000px}summary{cursor:pointer;font-size:12px;font-weight:650;color:#276c63;user-select:none}details[open]>summary{margin-bottom:18px}.finding-body{border-top:1px solid var(--line);margin-top:16px;padding-top:20px}dl{display:grid;grid-template-columns:155px 1fr;gap:14px 18px;margin:0 0 22px;font-size:13px}dt{font-weight:650;color:#3c6466}dd{margin:0}.repro{background:#edf5ee;padding:12px 15px;border-radius:6px}.evidence{margin-top:12px;border:1px solid var(--line);border-radius:5px;overflow:hidden}.evidence-label{background:#eff3ef;padding:8px 13px;font-size:11px}pre{font:11px/1.75 ui-monospace,SFMono-Regular,Consolas,monospace;margin:0;padding:15px 12px;overflow:auto;background:#f9faf7;color:#244651}pre .ln{display:inline-block;width:4em;color:#687d81;user-select:none}code{font-family:ui-monospace,SFMono-Regular,Consolas,monospace;font-size:.88em;overflow-wrap:anywhere}pre code{font-size:inherit;overflow-wrap:normal}.empty{padding:25px;background:#fff;border:1px dashed #c9d5ce;border-radius:8px}
.legend{display:flex;gap:18px;flex-wrap:wrap;font-size:11px;color:#496168;background:#e8efea;padding:12px 16px;border-radius:5px;margin-top:22px}.diagram-card{background:#fff;border:1px solid var(--line);border-radius:9px;padding:23px;margin-top:18px}.diagram-heading{display:flex;justify-content:space-between;gap:20px;align-items:center}.diagram-heading h3{margin:5px 0 7px;font-size:20px}.diagram-heading p{font-size:13px;color:var(--muted);margin-bottom:16px}.zoom-tools{display:flex;gap:5px}.zoom-tools button{background:#f5f7f3;border:1px solid #cad8d0;border-radius:5px;color:#305651;padding:4px 9px;font-size:12px}.diagram-viewport{overflow:auto;border-top:1px solid #e5ece6;border-bottom:1px solid #e5ece6;padding:16px 0;background:#fcfdfa}.diagram-inner{width:100%;min-width:620px}.diagram-inner svg{display:block;width:100%;height:auto}.diagram-note{font-size:12px;color:var(--muted);margin:18px 0 14px}.source-list{font-size:10px;margin:18px 0 0;color:var(--muted);overflow-wrap:anywhere}.dictionary{font-size:12px}.dictionary table{font-size:12px}.table-wrap{overflow:auto}table{width:100%;border-collapse:collapse;font-size:13px}th{font-size:10px;text-transform:uppercase;letter-spacing:1px;color:#526e70;text-align:left;background:#eaf0eb}th,td{padding:15px 16px;border-bottom:1px solid var(--line);vertical-align:top}td p{margin:7px 0 0;color:var(--muted);font-size:12px}.validation-table{background:#fff;border:1px solid var(--line);border-radius:8px;overflow:hidden}.validation-table td:first-child{width:27%}.pass{font-size:11px;color:#247451}.small-note{font-size:12px;color:var(--muted);margin-top:15px}.two-cols{display:grid;grid-template-columns:1fr 1fr;gap:22px;margin-top:24px}.text-card{padding:23px;border:1px solid var(--line);border-radius:8px;background:#fff}.text-card h3{font-size:17px}.text-card ul{margin:12px 0 0;padding-left:18px}.text-card li{font-size:12px;color:#4d656b;margin:10px 0}.roadmap{display:grid;grid-template-columns:1fr 1fr;gap:13px}.roadmap-item{display:flex;gap:18px;border:1px solid var(--line);background:#fff;border-radius:8px;padding:20px}.step-number{font-size:23px;font-weight:300;color:#418476}.roadmap h3{font-size:15px;margin-bottom:7px}.roadmap p{font-size:12px;color:var(--muted);margin-bottom:6px}.roadmap small{font-size:10px;color:#28776c}footer{border-top:1px solid var(--line);padding-top:20px;color:var(--muted);font-size:11px;display:flex;gap:20px;justify-content:space-between}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}[hidden]{display:none!important}
@media(max-width:1050px){:root{--navw:185px}aside{padding:28px 18px}main{padding:30px 25px}.architecture{grid-template-columns:1fr}.arch-card{padding:16px}.metrics{gap:8px}.metric{padding:14px}.metric b{font-size:29px}.two-cols{grid-template-columns:1fr}.diagram-card{padding:16px}}
@media(max-width:720px){aside{position:static;width:auto;padding:18px 20px;display:block}.side-caption{margin:0 0 12px}.brand{font-size:23px}nav{display:flex;overflow:auto;gap:16px}nav a{white-space:nowrap;padding:4px 0;font-size:11px;gap:6px}.side-bottom{display:none}main{margin:0;padding:26px 18px}.topline{margin-bottom:25px}.topline .eyebrow{font-size:9px;letter-spacing:1px}h1{font-size:42px;letter-spacing:-1.8px}.lede{font-size:15px}.metrics{grid-template-columns:1fr 1fr}.finding{padding:18px 16px}.finding h3{font-size:18px}.finding-meta{gap:7px;flex-wrap:wrap}.confidence{font-size:9px}dl{grid-template-columns:1fr;gap:5px}dd{margin-bottom:12px}.roadmap{grid-template-columns:1fr}.diagram-heading{align-items:flex-start}.zoom-tools{flex-shrink:0}.diagram-heading h3{font-size:17px}.section-top{align-items:flex-start}.section-top h2{font-size:25px}footer{display:block}.validation-table td{padding:12px 10px}}
@media(prefers-reduced-motion:reduce){html{scroll-behavior:auto}}
@media print{@page{size:A4;margin:14mm}body{font-size:10pt;background:white}aside,.topline button,.filters,.result-count,.zoom-tools,.skip,.no-print{display:none!important}main{margin:0;padding:0;max-width:none}h1{font-size:32pt}.hero{padding-bottom:12px;margin-bottom:20px}.metrics{margin:18px 0}.metric{padding:10px}.metric b{font-size:24pt}.finding{break-inside:avoid;padding:14px;margin:10px 0}.finding-details>summary{display:none}pre{white-space:pre-wrap;font-size:8pt}.diagram-card{break-before:page;break-inside:avoid;padding:12px}.diagram-inner{min-width:0;width:100%!important}.diagram-viewport{overflow:visible}.dictionary{display:none}.source-list{font-size:7pt}.section{margin-bottom:25px}.two-cols{grid-template-columns:1fr}.roadmap-item{break-inside:avoid}a{color:inherit;text-decoration:none}.table-wrap{overflow:visible}footer{font-size:8pt}}
`;

const script = `
const findings=[...document.querySelectorAll('.finding')];
const search=document.querySelector('#search'), priority=document.querySelector('#priority'), area=document.querySelector('#area');
function filter(){const q=search.value.toLowerCase().trim();let shown=0;for(const f of findings){f.hidden=!!((priority.value&&f.dataset.priority!==priority.value)||(area.value&&f.dataset.area!==area.value)||(q&&!f.textContent.toLowerCase().includes(q)));if(!f.hidden)shown++;}document.querySelector('#result-count').textContent=shown+' of '+findings.length+' findings shown';document.querySelector('#no-results').hidden=shown!==0;}
for(const el of [search,priority,area])el.addEventListener('input',filter);
let expand=false;document.querySelector('#expand').addEventListener('click',()=>{expand=!expand;for(const f of findings)if(!f.hidden)f.querySelector('details').open=expand;document.querySelector('#expand').textContent=expand?'Collapse all':'Expand all';});
for(const b of document.querySelectorAll('[data-zoom]'))b.addEventListener('click',()=>{const frame=b.closest('.diagram-card').querySelector('.diagram-inner');const old=Number(frame.dataset.scale||1);const scale=b.dataset.zoom==='reset'?1:Math.max(1,Math.min(3.5,old+(b.dataset.zoom==='in'?.5:-.5)));frame.dataset.scale=scale;frame.style.width=scale*100+'%';});
function showHash(){const target=document.getElementById(location.hash.slice(1));if(target?.classList.contains('finding')){search.value='';priority.value='';area.value='';filter();target.querySelector('details').open=true;target.scrollIntoView();}}
addEventListener('hashchange',showHash);showHash();
let printState=[];function beforePrint(){printState=findings.map(f=>({f,hidden:f.hidden,open:f.querySelector('details').open}));for(const f of findings){f.hidden=false;f.querySelector('details').open=true;}}
function afterPrint(){for(const s of printState){s.f.hidden=s.hidden;s.f.querySelector('details').open=s.open;}printState=[];}
addEventListener('beforeprint',beforePrint);addEventListener('afterprint',afterPrint);document.querySelector('#print').addEventListener('click',()=>window.print());
document.documentElement.dataset.reportReady='true';
`;

const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="description" content="Offline code review of vr_fire with nine source-backed findings and three entity relationship diagrams."><title>${esc(report.title)}</title><style>${css}</style></head><body>
<a class="skip" href="#main">Skip to report</a>
<aside><div class="brand">vr<span>_</span>fire</div><div class="side-caption">Engineering review</div><nav aria-label="Report sections"><a href="#overview"><span>01</span>Overview</a><a href="#findings"><span>02</span>Findings</a><a href="#data-model"><span>03</span>Data model / ERDs</a><a href="#validation"><span>04</span>Validation</a><a href="#next"><span>05</span>Remediation</a></nav><div class="side-bottom"><p><span class="dot"></span>Offline report<br>25 September 2026</p><p>Reviewed revision<br><code>${report.revision.slice(0,12)}</code></p><a href="README.md">Markdown version ↗</a></div></aside>
<main id="main"><div class="topline"><span class="eyebrow">California terrain · Rust workspace</span><button class="quiet-button" id="print">Print / save PDF</button></div>
<header class="hero" id="overview"><h1>Code, terrain<br>& <em>the connections.</em></h1><p class="lede">${esc(report.summary)}</p><div class="metrics"><div class="metric"><b>${report.findings.length}</b><span>Actionable findings</span><small>${p1} P1 · ${report.findings.length-p1} P2</small></div><div class="metric alert"><b>${p1}</b><span>High-priority issue</span><small>Relay resource bounds</small></div><div class="metric good"><b>${validation.passed}</b><span>Existing tests passed</span><small>${validation.ignored} download test ignored</small></div><div class="metric"><b>${reproduced}</b><span>Defects reproduced</span><small>Bounded offline probes</small></div></div><p class="scope"><strong>Review boundary.</strong> ${esc(report.scope)}</p><div class="callout"><strong>Start here:</strong> bound the relay's outbound queues (<a href="#F01">F01</a>), then preserve terrain validity through storage and packing (<a href="#F03">F03</a>). Passing unit tests leave these failure paths uncovered.</div></header>
<section class="section" aria-labelledby="architecture-title"><span class="eyebrow">System map</span><h2 id="architecture-title">Five crates, three operating paths</h2><div class="architecture"><div class="arch-card"><h3>01 / Offline terrain</h3><p>GeoJSON + USGS sources → EPSG:5070 store → baked meshes or compressed viewer patches.</p><code>vr_fire · compress-lab</code></div><div class="arch-card"><h3>02 / Interactive viewer</h3><p>Packed terrain with COG fallback, imagery mosaics, local truck physics and remote player poses.</p><code>viewer · Bevy · native / wasm</code></div><div class="arch-card"><h3>03 / Supporting services</h3><p>WebSocket forwarding and event stats; a separate worker fetches and caches requested lidar.</p><code>relay · hires · nginx / systemd</code></div></div><p class="small-note">No relational database is defined. The ERDs below describe concrete Rust structures, file records, map keys and their cardinalities.</p></section>
<section class="section" id="findings"><div class="section-top"><div><span class="eyebrow">Review register</span><h2>Findings with a path to resolution</h2></div><span class="pill">P1 = high · P2 = medium</span></div><p class="section-intro">“Reproduced” means a bundled synthetic probe observed the defect. “Code-confirmed” means the cause and reachable path were traced in source; end-to-end impact has not been measured. Each finding includes a trigger, recommendation and acceptance check.</p><div class="filters"><label class="sr-only" for="search">Search findings</label><input id="search" type="search" placeholder="Search findings, files or symptoms…"><label class="sr-only" for="priority">Priority</label><select id="priority"><option value="">All priorities</option><option>P1</option><option>P2</option></select><label class="sr-only" for="area">Component</label><select id="area"><option value="">All components</option>${[...new Set(report.findings.map(f=>f.area))].map(a=>`<option>${esc(a)}</option>`).join('')}</select><button class="quiet-button" id="expand">Expand all</button></div><p class="result-count" id="result-count" role="status" aria-live="polite">${report.findings.length} of ${report.findings.length} findings shown</p><noscript><p class="small-note">Search and zoom require JavaScript; all findings and diagrams remain readable without it.</p></noscript><div id="finding-list">${findingHTML}</div><p class="empty" id="no-results" hidden>No findings match these filters. Try a different search or component.</p></section>
<section class="section" id="data-model"><span class="eyebrow">Entity relationship diagrams</span><h2>Follow identity, ownership and provenance</h2><p class="section-intro">The diagrams describe the implemented model. K means a logical key in a filename, map or value, not an enforced database primary key. Open the text alternatives for a field dictionary and an explicit relationship list.</p><div class="legend"><span><strong>1</strong> exactly one</span><span><strong>0..1</strong> optional</span><span><strong>0..*</strong> zero or more</span><span><strong>Solid</strong> reference / containment</span><span><strong>Dashed</strong> derived / produced</span></div>${renderedDiagrams}</section>
<section class="section" id="validation"><span class="eyebrow">Evidence & limits</span><h2>What was actually checked</h2><p class="section-intro">${validation.passed} unique existing tests passed across the library, integration suites, viewer and services. Four additional probes reproduced defects; they are not counted as passing regression coverage.</p><div class="validation-table table-wrap"><table><thead><tr><th>Check / result</th><th>Command & interpretation</th></tr></thead><tbody>${checksHTML}</tbody></table></div><p class="small-note"><strong>Toolchain:</strong> ${esc(validation.toolchain)}</p><div class="two-cols"><div class="text-card"><h3>What is working well</h3><ul>${report.strengths.map(s=>`<li>${esc(s)}</li>`).join('')}</ul></div><div class="text-card"><h3>Outstanding verification</h3><ul>${report.limits.map(s=>`<li>${esc(s)}</li>`).join('')}</ul></div></div><details class="small-note"><summary>Compiler warnings and snapshot provenance</summary><ul>${validation.warnings.map(s=>`<li>${esc(s)}</li>`).join('')}</ul><p>${esc(report.snapshotNote)}</p><p>Source excerpts are embedded in this HTML and verified against the reviewed Git revision during generation. <a href="snapshot.json">Source hashes</a> · <a href="validation.json">Validation record</a> · <a href="probes/src/main.rs">Offline probes</a>.</p></details></section>
<section class="section" id="next"><span class="eyebrow">Suggested implementation order</span><h2>Turn the findings into bounded changes</h2><div class="roadmap">${remediations.map(([n,title,ids,body])=>`<div class="roadmap-item"><span class="step-number">${n}</span><div><h3>${title}</h3><p>${body}</p><small>${ids}</small></div></div>`).join('')}</div><p class="small-note">Recommendations are proposed follow-up work. Application behavior was not changed as part of this review.</p></section>
<footer><span>vr_fire / engineering review / ${report.date}</span><span>Self-contained HTML · embedded SVG · no network dependencies</span></footer></main><script>${script}</script></body></html>
`;
writeFileSync(resolve(here,'index.html'),html);

const md = `# vr_fire code review — ${report.date}

[Open the HTML report](index.html). It works directly from disk, including diagrams, filters, source excerpts and print styling.

Revision: \`${report.revision}\`.

${report.summary}

${report.scope}

${report.snapshotNote}

## Findings

| ID | Priority | Component | Evidence | Finding |
|---|---|---|---|---|
${report.findings.map(f=>`| [${f.id}](index.html#${f.id}) | ${f.priority} | ${f.area} | ${f.confidence} | ${f.title} |`).join('\n')}

P1 = high priority; P2 = medium priority. Reproduced = synthetic local probe. Code-confirmed = source-traced cause and path, without end-to-end load or visual validation.

${report.findings.map(f=>`### ${f.id} [${f.priority}] ${f.title}\n\n${f.impact}\n\n**Trigger:** ${f.trigger}\n\n**Cause:** ${f.detail}\n\n**Recommendation:** ${f.recommendation}\n\n**Acceptance:** ${f.acceptance}\n\n${f.reproduction?`**Reproduction:** ${f.reproduction}\n\n`:''}**Source:** ${f.evidence.map(e=>`[${e.file}:${e.start}–${e.end}](../../${e.file})`).join(', ')}.\n`).join('\n')}
## Data model / ERDs

There is no relational database. Keys and cardinalities describe files, Rust maps and runtime values, not SQL constraints. The HTML embeds all SVGs and provides field dictionaries and text relationship lists.

${diagrams.map(d=>`### ${d.title}\n\n${d.subtitle}\n\n![${d.title}](diagrams/${d.id}.svg)\n\n${d.notes}\n\n[Editable DOT](diagrams/${d.id}.dot). Source: ${d.sources.map(s=>`[${s}](../../${s})`).join(', ')}.\n`).join('\n')}
## Validation

${validation.passed} unique existing tests passed; ${validation.ignored} external-download test ignored. ${validation.probes} bounded defect probes reproduced the documented problems.

${validation.checks.map(c=>`- **${c.name}: ${c.result}.**\n\n  \`${c.command}\`\n\n  ${c.note}\n`).join('\n')}
Toolchain: ${validation.toolchain}.

### Compiler warnings

${validation.warnings.map(s=>`- ${s}`).join('\n')}

### Strengths

${report.strengths.map(s=>`- ${s}`).join('\n')}

### Limitations

${report.limits.map(s=>`- ${s}`).join('\n')}

## Maintaining this report

Content is in [report.json](report.json), [diagrams.json](diagrams.json) and [validation.json](validation.json). Regenerate HTML, Markdown, SVG and DOT with Node.js and Graphviz installed:

\`\`\`sh
node docs/review/build.mjs
\`\`\`

Generation verifies referenced source files against the pinned revision and fails if they differ. [snapshot.json](snapshot.json) records their SHA-256 hashes. Test results are recorded evidence, not rerun by the generator.

The probes use temporary files, make no HTTP calls, and intentionally assert current defective behavior. Run them with the command above; do not treat them as tests that should stay green after fixing the findings.
`;
writeFileSync(resolve(here,'README.md'),md);
writeFileSync(resolve(here,'snapshot.json'),JSON.stringify({revision:report.revision,date:report.date,files:[...sourceCache].sort(([a],[b])=>a.localeCompare(b)).map(([file,content])=>({file,sha256:createHash('sha256').update(content).digest('hex')}))},null,2)+'\n');
console.log(`Built ${relative(root,resolve(here,'index.html'))}: ${report.findings.length} findings, ${diagrams.length} embedded ERDs, ${sourceCache.size} verified source files.`);
