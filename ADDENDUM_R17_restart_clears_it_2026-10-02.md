# Addendum to Brief R17 — restarting the app clears it

Date: 2026-10-02

One observation was missing from the brief as sent.

## Reported

In the TUI, the five tracks stay `Failed` across repeated attempts within the
same session. Closing the application and starting it again — in this instance
after a recompile — and the conversion works.

## Alongside it

A fresh CLI process run with the user's configured folder template fails every
time. The binary used was built after the most recent code change, so a new
process by itself does not clear the condition.

Taken together: restarting the application clears it; starting a new process
with that template does not.
