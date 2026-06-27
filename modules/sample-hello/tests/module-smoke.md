# Sample Hello Smoke

1. `ojosctl module validate modules/sample-hello/module.yaml`
2. `ojosctl module package modules/sample-hello -o .tmp/agent/scratch/sample-hello.ojosmod`
3. `ojosctl module verify .tmp/agent/scratch/sample-hello.ojosmod`
4. Install dry-run, install apply, enable, inspect Runtime Snapshot, then disable.
