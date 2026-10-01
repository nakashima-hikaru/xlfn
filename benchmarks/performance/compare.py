#!/usr/bin/env python3
"""Compare already-built Criterion benches; compilation is never timed."""
import argparse, hashlib, json, os, platform, shutil, statistics, subprocess
from pathlib import Path
parser = argparse.ArgumentParser(description="Run frozen Criterion executables in ABBA order.")
parser.add_argument("--baseline-build", type=Path, required=True)
parser.add_argument("--candidate-build", type=Path, required=True)
parser.add_argument("--work-dir", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--baseline-revision", required=True)
parser.add_argument("--filter", action="append", metavar="BENCH=REGEX",
                    help="Override the default cases; repeat once per benchmark executable.")
parser.add_argument("--measurement-seconds", type=float, default=1.0)
parser.add_argument("--warmup-seconds", type=float, default=0.3)
parser.add_argument("--samples", type=int, default=50)
args = parser.parse_args()
ROOT = args.work_dir.resolve()
ROOT.mkdir(parents=True, exist_ok=True)
if (ROOT / "binaries").exists():
    parser.error("use a fresh --work-dir to retain each executable snapshot")
args.output.parent.mkdir(parents=True, exist_ok=True)

def executables(path):
    result = {}
    for line in path.read_text().splitlines():
        r = json.loads(line)
        if r.get('executable'):
            result[r['target']['name']] = Path(r['executable'])
    return result
baseline = executables(args.baseline_build)

candidate = executables(args.candidate_build)
filters = {
 'input_identity': r'^input_identity/(f64|matrix_f64_(16|256|4096|100k)|eight_matrix_f64_4096|utf16/(ascii_short|unicode_1k))$',
 'argument_ingress': r'^argument_ingress/(f64/with_identity|matrix_f64_100k/(plain|with_identity|prepare_identity)|matrix_f64_(1|16|1k)/prepare_identity|vec_f64_100k/with_identity|excel_value_matrix_100k/with_identity|matrix_string_10k/borrowed|handle/with_identity)$',
 'object_lease': r'^object_lease/(pin_acquire_release_serial|final_pin_release|(same_object|distinct_objects)/(1|4|16))$',
 'formula_revision': r'^formula_revision/warm_hit/(f64|matrix_f64_100k)$',
}
if args.filter:
 filters = {}
 for specification in args.filter:
  if '=' not in specification:
   parser.error('--filter must have the form BENCH=REGEX')
  bench, pattern = specification.split('=', 1)
  if not bench or not pattern or bench in filters:
   parser.error('each --filter needs a nonempty, unique benchmark and regex')
  filters[bench] = pattern
if args.measurement_seconds <= 0 or args.warmup_seconds <= 0 or args.samples < 10:
 parser.error('measurement/warmup times must be positive and samples must be at least 10')
record = {'baseline_revision': args.baseline_revision, 'host': platform.system() + ' ' + platform.machine(), 'toolchain': subprocess.check_output(['rustc','--version'], text=True).strip(), 'features': ['bench-internals','async','cache'], 'warmup_seconds': args.warmup_seconds, 'measurement_seconds': args.measurement_seconds, 'samples': args.samples, 'filters': filters, 'order': ['baseline','candidate','candidate','baseline'], 'executable_sha256': {}, 'runs': []}
for kind, binaries in [('baseline',baseline),('candidate',candidate)]:
 for bench in filters:
  frozen = ROOT / 'binaries' / kind / bench
  frozen.parent.mkdir(parents=True, exist_ok=True)
  shutil.copy2(binaries[bench], frozen)
  frozen.chmod(0o555)
  binaries[bench] = frozen
  record['executable_sha256'][kind+'/'+bench] = hashlib.sha256(frozen.read_bytes()).hexdigest()
expected_cases = {}
for round_number, kind in enumerate(record['order']):
 for bench, pattern in filters.items():
  out = ROOT / 'adopted-measurements' / str(round_number) / kind / bench
  out.mkdir(parents=True, exist_ok=True)
  env = dict(os.environ, CARGO_TARGET_DIR=str(out), XLFN_BENCH_MEASUREMENT_MS=str(max(1, round(args.measurement_seconds * 1000))))
  binary = (baseline if kind=='baseline' else candidate)[bench]
  assert hashlib.sha256(binary.read_bytes()).hexdigest() == record['executable_sha256'][kind+'/'+bench]
  print('Running', round_number, kind, bench, flush=True)
  with (out/'stdout.log').open('w') as log:
   subprocess.run([str(binary), pattern, '--bench', '--noplot', '--warm-up-time', str(args.warmup_seconds), '--sample-size', str(args.samples)], env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
  cases = set()
  for estimates in out.rglob('new/estimates.json'):
   identity = json.loads((estimates.parent/'benchmark.json').read_text())['full_id']
   cases.add(identity)
   data = json.loads(estimates.read_text())
   record['runs'].append({'round': round_number, 'kind':kind, 'benchmark':identity, 'median_ns':data['median']['point_estimate'], 'mean_ns':data['mean']['point_estimate'], 'mean_95_ci':data['mean']['confidence_interval']})
  if not cases or cases != expected_cases.setdefault(bench, cases):
   raise RuntimeError(f'{kind}/{bench} produced empty or mismatched benchmark cases')
  args.output.write_text(json.dumps(record,indent=2)+'\n')
for case in sorted({r['benchmark'] for r in record['runs']}):
 before = statistics.median([r['median_ns'] for r in record['runs'] if r['benchmark']==case and r['kind']=='baseline'])
 after = statistics.median([r['median_ns'] for r in record['runs'] if r['benchmark']==case and r['kind']=='candidate'])
 print(case, round(before,2), round(after,2), round((after/before-1)*100,2), flush=True)
record['summary'] = []
for case in sorted({r['benchmark'] for r in record['runs']}):
 before = statistics.median([r['median_ns'] for r in record['runs'] if r['benchmark']==case and r['kind']=='baseline'])
 after = statistics.median([r['median_ns'] for r in record['runs'] if r['benchmark']==case and r['kind']=='candidate'])
 record['summary'].append({'benchmark':case,'baseline_ns':before,'candidate_ns':after,'change_percent':(after/before-1)*100})
args.output.write_text(json.dumps(record,indent=2)+'\n')
