#!/usr/bin/env python3
"""Fail if the D1001 OTA application image does not fit its partition slot.

The budget is the smaller of the ota_0 and ota_1 sizes in the partition table.
The image is produced with `espflash save-image` (the ELF segments overlap, so
the ELF size says nothing). The unmerged output is byte-identical to the part
of the merged flash image that scripts/flash-app.sh writes at the ota_0 offset.
"""
import argparse
import csv
import pathlib
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
DEFAULT_ELF = ("target/build/p4-build/linux/cargo/riscv32imafc-esp-espidf/"
               "release/njorun-p4-smoke")
DEFAULT_TABLE = "experiments/p4-build/partitions.d1001.csv"


def parse_number(text):
    return int(text.strip(), 0)


def slot_budget(table):
    """Return the smallest OTA app slot size in bytes from a partition CSV."""
    sizes = {}
    lines = (line for line in pathlib.Path(table).read_text().splitlines()
             if line.strip() and not line.lstrip().startswith("#"))
    for row in csv.reader(lines):
        cells = [cell.strip() for cell in row]
        if len(cells) >= 5 and cells[1] == "app" and cells[2].startswith("ota_"):
            sizes[cells[0]] = parse_number(cells[4])
    if len(sizes) < 2:
        raise SystemExit(f"{table}: expected at least two OTA app slots, found {sorted(sizes)}")
    return min(sizes.values()), sizes


def evaluate(image_size, budget):
    headroom = budget - image_size
    return headroom >= 0, headroom


def report(image_size, budget):
    ok, headroom = evaluate(image_size, budget)
    return ok, (f"OTA image: {image_size:,} bytes\n"
                f"Slot budget: {budget:,} bytes\n"
                f"Headroom: {headroom:,} bytes ({headroom / budget:.2%} of the slot)\n"
                f"Used: {image_size / budget:.2%}")


# espflash refuses to write an image that does not fit its slot and then prints
# no size we could report. Measure against one oversized slot (the whole flash
# after the bootloader area) so that this script alone decides pass or fail
# against the real budget.
MEASURE_TABLE = "ota_0, app, ota_0, 0x20000, 0x1000000,\n"


def build_image(elf, out):
    table = out.with_name("measure.csv")
    table.write_text(MEASURE_TABLE)
    subprocess.run(["espflash", "save-image", "--chip", "esp32p4", "--flash-size", "32mb",
                    "--partition-table", str(table), str(elf), str(out)],
                   check=True, stdout=subprocess.DEVNULL)
    return out.stat().st_size


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--elf", type=pathlib.Path, default=ROOT / DEFAULT_ELF)
    parser.add_argument("--partition-table", type=pathlib.Path, default=ROOT / DEFAULT_TABLE)
    args = parser.parse_args(argv)
    budget, sizes = slot_budget(args.partition_table)
    with tempfile.TemporaryDirectory() as directory:
        size = build_image(args.elf, pathlib.Path(directory) / "app.bin")
    ok, text = report(size, budget)
    print(text)
    if not ok:
        print(f"::error::the OTA image exceeds its slot by {-evaluate(size, budget)[1]:,} bytes",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
