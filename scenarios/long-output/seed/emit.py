import sys

# Padding is long on purpose. The value to report is only the last line.
line = "pad-" + ("x" * 60)
for i in range(1400):
    sys.stdout.write(f"{i:04d}:{line}\n")
sys.stdout.write("ANSWER=kestrel-4821\n")
