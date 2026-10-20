from pathlib import Path
import os, subprocess, json, hashlib, statistics
w=Path(__file__).parent
manifest=json.loads((w/'executables.json').read_text())
filters={"argument_ingress":r"^argument_ingress/(f64/plain|string_(short/(plain|borrowed)|unicode/(owned|borrowed)|inline_.*/borrowed|1k/(plain|borrowed)|ascii_4k/owned|unicode_1k/(owned|borrowed)|unicode_limit/owned|japanese_4k/(owned|borrowed)|unicode_sparse/borrowed)|matrix_string_(100|10k)/(owned|borrowed))$", "array_string_output":r"^(scalar_output/.*|array_string_output/(borrowed_str|unicode_short|unicode_1k|ascii_1k|mixed|empty)/16384|callback_counted_utf16/.*)$"}
rows=[]
for i,v in enumerate(['candidate','baseline','baseline','candidate']):
 for bench, pattern in filters.items():
  exe=w/'frozen'/f'{v}-{bench}'
  assert hashlib.sha256(exe.read_bytes()).hexdigest()==manifest[str(exe)]['sha256']
  out=w/'outlined-runs'/f'{i}-{v}'/bench;out.mkdir(parents=True,exist_ok=True)
  env=dict(os.environ,CRITERION_HOME=str(out),XLFN_BENCH_MEASUREMENT_MS='500')
  with (out/'run.log').open('w') as log:
   subprocess.run(['rtk','proxy',str(exe),'--bench',pattern,'--warm-up-time','0.1','--sample-size','30','--nresamples','1000','--noplot'],env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
  for p in out.rglob('new/estimates.json'):
   e=json.loads(p.read_text()); key=str(p.parent.parent.relative_to(out))
   rows.append(dict(round=i,variant=v,case=key,median_ns=e['median']['point_estimate']))
 (w/'paired-raw.json').write_text(json.dumps(rows,indent=2))
 print('Finished',i,v,flush=True)
summary=[]
for key in sorted({r['case'] for r in rows}):
 vals={v:[r['median_ns'] for r in rows if r['case']==key and r['variant']==v] for v in ['baseline','candidate']}
 b=statistics.median(vals['baseline']);c=statistics.median(vals['candidate'])
 summary.append(dict(case=key,baseline_ns=b,candidate_ns=c,change_percent=100*(c/b-1),runs=vals))
(w/'paired-summary.json').write_text(json.dumps(summary,indent=2))
print(json.dumps(summary,indent=2))
