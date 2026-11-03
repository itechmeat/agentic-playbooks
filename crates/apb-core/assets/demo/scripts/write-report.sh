#!/bin/sh
# Writes report.md in the working directory (the demo directory): the run
# id and a fixed text. Only printf; nothing else is started or read.
printf '# apb demo report\n\nRun: %s\n\nThe script node `write_report` wrote this file. Nothing else ran:\nno agent, no connector, no network.\n' "$APB_RUN_ID" > report.md
printf 'REPORT_WRITTEN report.md for run %s\n' "$APB_RUN_ID"
