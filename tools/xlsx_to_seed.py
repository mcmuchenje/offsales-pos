"""Convert the legacy 'Off sales.xlsx' tracking sheet into migrations/seed.sql.

Sheet layout: every product occupies 3 columns: [date, qty sold, running stock].
Row 1 = "<Name> <size> $<price>", row 2 = opening stock in the 3rd column,
rows 3.. = one sale per row (date is a frozen TODAY() timestamp).

Usage:  python tools/xlsx_to_seed.py "Off sales.xlsx" migrations/seed.sql
"""
import re
import sys
import openpyxl

src, dst = sys.argv[1], sys.argv[2]
ws = openpyxl.load_workbook(src, data_only=True).active

def q(s):
    return "'" + str(s).replace("'", "''") + "'"

out = ["-- Generated from legacy 'Off sales.xlsx' by tools/xlsx_to_seed.py"]
sale_id = 0
sales = []  # (date, row, product_id, qty, price_cents)
for pid, col in enumerate(range(1, ws.max_column + 1, 3), start=1):
    header = ws.cell(1, col).value
    if not header:
        continue
    header = str(header).strip()
    m = re.search(r"\$\s*([\d.]+)\s*$", header)
    price_cents = round(float(m.group(1)) * 100) if m else 0
    name = header[: m.start()].strip() if m else header
    opening = int(ws.cell(2, col + 2).value or 0)
    out.append(
        f"INSERT INTO products (id, name, price_cents, stock_qty, active) "
        f"VALUES ({pid}, {q(name)}, {price_cents}, {opening}, 1);"
    )
    out.append(
        f"INSERT INTO stock_movements (product_id, change_qty, reason, note, created_at) "
        f"VALUES ({pid}, {opening}, 'opening', 'Imported opening stock', '2026-10-25 00:00:00');"
    )
    for r in range(3, ws.max_row + 1):
        qty = ws.cell(r, col + 1).value
        if qty in (None, "", 0):
            continue
        d = ws.cell(r, col).value
        sales.append((d.strftime("%Y-%m-%d"), r, pid, int(qty), price_cents))

# Order by date then sheet row so ids are chronological-ish. Times are unknown in
# the sheet, so give each imported sale a synthetic time that preserves order.
sales.sort(key=lambda s: (s[0], s[1], s[2]))
per_day = {}
for date, _row, pid, qty, price in sales:
    sale_id += 1
    n = per_day.get(date, 0)
    per_day[date] = n + 1
    ts = f"{date} {8 + n // 60 % 14:02d}:{n % 60:02d}:00"
    total = qty * price
    out.append(
        f"INSERT INTO sales (id, created_at, total_cents, tendered_cents, change_cents, payment_method, note) "
        f"VALUES ({sale_id}, '{ts}', {total}, {total}, 0, 'cash', 'Imported from spreadsheet');"
    )
    out.append(
        f"INSERT INTO sale_items (sale_id, product_id, qty, unit_price_cents, line_total_cents) "
        f"VALUES ({sale_id}, {pid}, {qty}, {price}, {total});"
    )
    out.append(
        f"INSERT INTO stock_movements (product_id, change_qty, reason, ref_id, created_at) "
        f"VALUES ({pid}, {-qty}, 'sale', {sale_id}, '{ts}');"
    )
    out.append(f"UPDATE products SET stock_qty = stock_qty - {qty} WHERE id = {pid};")

with open(dst, "w", encoding="utf-8", newline="\n") as f:
    f.write("\n".join(out) + "\n")
print(f"wrote {dst}: {sale_id} sales")
