# oxim-sim

Simulated analyzers, LIS systems and point-of-care devices for testing [OXIM](../../README.md) without hardware. All data is synthetic.

```text
oxim-sim generate hl7-oru --count 3 --results 5          # print synthetic ORU^R01 messages
oxim-sim generate astm --count 100 --out ./samples       # write ASTM result files

oxim-sim mllp receive --listen 127.0.0.1:2576            # play the LIS: acknowledge everything
oxim-sim mllp receive --listen 127.0.0.1:2576 --fail-every 5 --delay-ms 200
oxim-sim mllp send --to 127.0.0.1:2575 --count 1000 --rate 200   # load test with latency percentiles
oxim-sim mllp send --to 127.0.0.1:2575 --file result.hl7 --verbose

oxim-sim astm send --to 127.0.0.1:5100 --count 10        # play an analyzer over LIS01
oxim-sim astm send --listen 0.0.0.0:5100 --file run.astm # analyzer as the server side
oxim-sim astm receive --listen 0.0.0.0:5101              # play a host receiving worklists

oxim-sim poct send --to 127.0.0.1:7000 --observations 5  # HEL, DST, OBS..., EOT, END
```

Files may use LF or CRLF line endings; they are converted to the CR separators HL7 and ASTM require.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
