# Runs in the project root. `work.started` tells the harness the node is
# running; while `hold` exists the node keeps running, so the harness can
# pause the run or kill its driver mid-node.
: > work.started
while [ -f hold ]; do sleep 0.05; done
echo work done
