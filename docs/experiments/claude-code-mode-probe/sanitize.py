# Turns the probe runs (<run>-out.json, <run>-hooks.jsonl, <run>-mod/ beside
# this script) into the fixtures under fixtures/conversation/claude/code-mode.
# Usage: python3 -I sanitize.py <fixtures dir>
import json, os, sys, glob, re
SCR = os.path.dirname(os.path.abspath(__file__))
WORK = SCR + '/work'
HOME = os.path.expanduser('~')
OUT = sys.argv[1]
KEEP = {'user', 'assistant', 'system', 'repl-mode', 'tools-used', 'host-row'}
DROP_KEYS = {'requestRef', 'requestId', 'gitBranch', 'dataFlowOn', 'advisorModel', 'wireToolInputs', 'session-owner'}

def scrub(v):
    if isinstance(v, str):
        v = v.replace(WORK, '/Users/person/project').replace(SCR, '/Users/person/probe')
        v = v.replace(HOME, '/Users/person')
        v = re.sub('-Users-' + re.escape(os.path.basename(HOME)) + r'[^/"]*', '-Users-person-project', v)
        return v
    if isinstance(v, dict):
        return {k: scrub(x) for k, x in v.items() if k not in DROP_KEYS}
    if isinstance(v, list):
        return [scrub(x) for x in v]
    return v

def transcript(sid):
    return glob.glob(f'{HOME}/.claude/projects/*/{sid}.jsonl')[0]

cases = {
  'repl-parallel': 'a', 'refused-call': 'b', 'script-error': 'e',
}
for case, run in cases.items():
    d = os.path.join(OUT, case); os.makedirs(d, exist_ok=True)
    sid = json.load(open(f'{SCR}/{run}-out.json'))['session_id']
    rows = []
    for line in open(transcript(sid)):
        r = json.loads(line)
        if r.get('type') not in KEEP: continue
        rows.append(scrub(r))
    with open(f'{d}/transcript.jsonl', 'w') as f:
        for r in rows: f.write(json.dumps(r) + '\n')
    with open(f'{d}/hooks.jsonl', 'w') as f:
        for line in open(f'{SCR}/{run}-hooks.jsonl'):
            r = json.loads(line)
            if r.get('hook_event_name') == 'SessionStart': continue
            f.write(json.dumps(scrub(r)) + '\n')
    mod = os.path.join(SCR, f'{run}-mod')
    if os.path.isdir(mod):
        with open(f'{d}/mod-events.jsonl', 'w') as f:
            for name in sorted(os.listdir(mod)):
                f.write(json.dumps(scrub(json.load(open(os.path.join(mod, name))))) + '\n')
print('ok')
