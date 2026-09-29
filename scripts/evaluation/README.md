# MCP read-probe baseline

`mcp_baseline.py` is a domain-neutral evaluator over the actual authenticated
MCP endpoint. A domain supplies a native project, its SHA256, expected read
responses and named unmeasured gates. This is not an authoring language or a
substitute for interaction, performance or independent-agent acceptance.

Run with Python3 (standard library only):

```sh
python3 scripts/evaluation/mcp_baseline.py /absolute/domain/manifest.json \
  --credentials /private/runtime/access.json --instance disposable-instance \
  --replace-disposable-document --output /tmp/baseline-report.json \
  --source core=FULL_SHA --source domain=FULL_SHA --source apps=FULL_SHA
```

The credential JSON contains `url` and `token` obtained through normal app
pairing. It is read once and never copied or printed. The runner opens its own
MCP session and closes it on exit. It confirms the expected instance before
loading the fixture. The explicit replacement flag authorizes discarding that
disposable instance's current document. Never point it at a working document.

Only allowlisted read tools may appear in manifest checks. Expected maps are
subsets, arrays have exact shape/order, scalar values match exactly unless the
check explicitly declares an absolute numeric tolerance. Boolean/string values
are not coerced into numbers. Tool errors remain failed gates and later probes
still run. Bootstrap failure leaves subsequent gates NOT_RUN.

Reports retain gate results, declared source revisions, fixture/guidance digests
and per-request response bytes/elapsed time. They exclude credentials and raw
responses. Review reports before committing. HTTP/MCP elapsed time is not an
input-to-render measurement. `full_acceptance` is always false: this runner
cannot certify the complete human-agent episode. Exit1 means a failed gate;
exit2 means no observed failures but acceptance remains untested.

```sh
python3 -m unittest discover -s scripts/evaluation -p 'test_*.py'
```
