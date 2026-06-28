# Sample Hello 冒烟检查

1. `ojosctl module validate modules/sample-hello/module.yaml`
2. `ojosctl module package modules/sample-hello -o .tmp/agent/scratch/sample-hello.ojosmod`
3. `ojosctl module verify .tmp/agent/scratch/sample-hello.ojosmod`
4. 执行 install dry-run、install apply、enable、Runtime Snapshot 检查，然后 disable。
