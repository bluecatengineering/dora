#!/usr/bin/env python3
"""
Dora DHCP Client Compatibility Matrix -- Result Formatter

Reads JSON result files produced by the dhcp-client-matrix NixOS tests,
combines standalone and NATS results into a unified matrix, and produces:

  - Terminal output with ANSI colours (default)
  - GitHub-flavoured Markdown table (--output-md)
  - Combined JSON (--output-json)
  - Baseline comparison / regression detection (--baseline)

Usage:
  python3 format-matrix-results.py \\
    --standalone result/standalone-results.json \\
    --nats result/nats-results.json \\
    --output-json combined.json \\
    --output-md matrix.md \\
    [--baseline previous/combined.json]
"""

import argparse
import json
import os
import sys
from datetime import datetime, timezone

# ── Constants ──────────────────────────────────────────────────────────────

# Column order in the matrix
TEST_COLS = ["lease", "options", "renew", "release", "load"]

# ANSI colour codes
C_RESET  = "\033[0m"
C_BOLD   = "\033[1m"
C_GREEN  = "\033[32m"
C_RED    = "\033[1;31m"
C_YELLOW = "\033[33m"
C_CYAN   = "\033[36m"
C_DIM    = "\033[2m"
C_BLUE   = "\033[34m"

# Status symbols
SYM_PASS = f"{C_GREEN}  pass  {C_RESET}"
SYM_FAIL = f"{C_RED}  FAIL  {C_RESET}"
SYM_SKIP = f"{C_YELLOW}  skip  {C_RESET}"
SYM_NA   = f"{C_DIM}   --   {C_RESET}"
SYM_NEW_PASS = f"{C_GREEN}  +pass {C_RESET}"
SYM_REGR     = f"{C_RED}  !REGR {C_RESET}"

# Plain symbols for Markdown
P_PASS = "pass"
P_FAIL = "**FAIL**"
P_SKIP = "skip"
P_NA   = " -- "


# ── Helpers ────────────────────────────────────────────────────────────────

def load_json(path):
    if not path or not os.path.exists(path):
        return None
    with open(path) as f:
        return json.load(f)


def status_of(tests_dict, test_name):
    """Extract the status string for a given test from the results dict."""
    if test_name not in tests_dict:
        return None
    return tests_dict[test_name].get("status")


def col_name(test_name):
    """Map test names like v4_lease, v6_lease, load to column names."""
    if test_name.startswith("v4_") or test_name.startswith("v6_"):
        return test_name.split("_", 1)[1]
    return test_name


def iter_client_rows(result_data):
    """Yield (client_name, protocol, tests_dict) from a single backend result."""
    if not result_data:
        return
    for cname in sorted(result_data.get("clients", {})):
        for proto in sorted(result_data["clients"][cname]):
            yield cname, proto, result_data["clients"][cname][proto]


def sym(status, plain=False):
    if status is None:
        return P_NA if plain else SYM_NA
    return {
        "pass": P_PASS if plain else SYM_PASS,
        "fail": P_FAIL if plain else SYM_FAIL,
        "skip": P_SKIP if plain else SYM_SKIP,
    }.get(status, status)


# ── Combined matrix ───────────────────────────────────────────────────────

def build_combined(standalone, nats):
    """Build a combined data structure suitable for rendering."""
    combined = {
        "meta": {
            "generated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "backends": [],
        },
        "rows": [],        # [{client, proto, standalone: {col: status}, nats: {col: status}}]
        "summary": {},
    }

    # Collect all (client, proto) pairs across both backends
    seen = {}
    for label, data in [("standalone", standalone), ("nats", nats)]:
        if not data:
            continue
        combined["meta"]["backends"].append(label)
        for cname, proto, tests in iter_client_rows(data):
            key = (cname, proto)
            if key not in seen:
                seen[key] = {"client": cname, "proto": proto, "standalone": {}, "nats": {}}
            for tname, tval in tests.items():
                col = col_name(tname)
                seen[key][label][col] = tval.get("status")

    combined["rows"] = [seen[k] for k in sorted(seen)]

    # Summaries per backend
    for label, data in [("standalone", standalone), ("nats", nats)]:
        if data:
            combined["summary"][label] = data.get("summary", {})

    return combined


# ── Terminal rendering ─────────────────────────────────────────────────────

def render_terminal(combined, baseline_combined=None):
    backends = combined["meta"]["backends"]
    lines = []

    width = 80
    if len(backends) == 2:
        width = 110

    lines.append("")
    lines.append(f"{C_BOLD}{'=' * width}{C_RESET}")
    lines.append(f"{C_BOLD}{C_CYAN}  Dora DHCP Client Compatibility Matrix{C_RESET}")
    lines.append(f"  Generated: {combined['meta']['generated']}")
    lines.append(f"{C_BOLD}{'=' * width}{C_RESET}")
    lines.append("")

    # Header
    name_w = 20
    proto_w = 5
    col_w = 8

    hdr = f"  {'Client':<{name_w}} {'Proto':<{proto_w}}"
    sep = f"  {'-' * name_w} {'-' * proto_w}"
    for backend in backends:
        hdr += f" {C_BOLD}|{C_RESET} "
        sep += " + "
        for c in TEST_COLS:
            hdr += f"{c:^{col_w}}"
            sep += f"{'-' * col_w}"
        hdr += f" {C_DIM}[{backend}]{C_RESET}"
        sep += f" {'.' * (len(backend) + 2)}"

    lines.append(hdr)
    lines.append(sep)

    # Rows
    for row in combined["rows"]:
        line = f"  {row['client']:<{name_w}} {row['proto']:<{proto_w}}"
        for backend in backends:
            line += f" {C_DIM}|{C_RESET} "
            cols = row.get(backend, {})
            for c in TEST_COLS:
                st = cols.get(c)
                # Check for regression vs baseline
                if baseline_combined:
                    old_st = _baseline_status(baseline_combined, row["client"], row["proto"], backend, c)
                    if old_st == "pass" and st == "fail":
                        line += SYM_REGR
                        continue
                    if old_st != "pass" and st == "pass":
                        line += SYM_NEW_PASS
                        continue
                line += sym(st)
        lines.append(line)

    # Summary
    lines.append("")
    lines.append(f"{C_BOLD}{'=' * width}{C_RESET}")
    for backend in backends:
        s = combined["summary"].get(backend, {})
        lines.append(
            f"  {C_BOLD}[{backend}]{C_RESET}  "
            f"Total: {s.get('total', 0)}  "
            f"{C_GREEN}Passed: {s.get('passed', 0)}{C_RESET}  "
            f"{C_RED}Failed: {s.get('failed', 0)}{C_RESET}  "
            f"{C_YELLOW}Skipped: {s.get('skipped', 0)}{C_RESET}"
        )
    lines.append(f"{C_BOLD}{'=' * width}{C_RESET}")

    # Legend
    lines.append(f"  {SYM_PASS}= pass  {SYM_FAIL}= FAIL  {SYM_SKIP}= skip  {SYM_NA}= N/A")
    if baseline_combined:
        lines.append(f"  {SYM_NEW_PASS}= new pass  {SYM_REGR}= regression")
    lines.append("")

    return "\n".join(lines)


# ── Markdown rendering ─────────────────────────────────────────────────────

def render_markdown(combined, baseline_combined=None):
    backends = combined["meta"]["backends"]
    lines = []

    lines.append("## Dora DHCP Client Compatibility Matrix")
    lines.append(f"_Generated: {combined['meta']['generated']}_\n")

    # Header
    if len(backends) == 1:
        hdr = "| Client | Proto |"
        sep = "|--------|-------|"
        for c in TEST_COLS:
            hdr += f" {c} |"
            sep += "------|"
    else:
        hdr = "| Client | Proto |"
        sep = "|--------|-------|"
        for backend in backends:
            for c in TEST_COLS:
                hdr += f" {c} |"
                sep += "------|"
        # Add a note about column grouping
        lines.append(f"Backends tested: {', '.join(backends)}\n")

    lines.append(hdr)
    lines.append(sep)

    # Rows
    for row in combined["rows"]:
        line = f"| {row['client']} | {row['proto']} |"
        for backend in backends:
            cols = row.get(backend, {})
            for c in TEST_COLS:
                st = cols.get(c)
                cell = sym(st, plain=True)
                # Regression marker
                if baseline_combined:
                    old_st = _baseline_status(baseline_combined, row["client"], row["proto"], backend, c)
                    if old_st == "pass" and st == "fail":
                        cell = "**REGR**"
                    elif old_st != "pass" and st == "pass":
                        cell = "+pass"
                line += f" {cell} |"
        lines.append(line)

    # Summary
    lines.append("")
    for backend in backends:
        s = combined["summary"].get(backend, {})
        lines.append(
            f"**[{backend}]** "
            f"Total: {s.get('total', 0)} | "
            f"Passed: {s.get('passed', 0)} | "
            f"Failed: {s.get('failed', 0)} | "
            f"Skipped: {s.get('skipped', 0)}"
        )

    return "\n".join(lines)


# ── Baseline comparison ────────────────────────────────────────────────────

def _baseline_status(baseline, client, proto, backend, col):
    """Look up a status in a baseline combined structure."""
    for row in baseline.get("rows", []):
        if row["client"] == client and row["proto"] == proto:
            return row.get(backend, {}).get(col)
    return None


def render_diff(combined, baseline):
    """Produce a diff summary between baseline and current."""
    lines = [f"\n{C_BOLD}Changes from baseline:{C_RESET}"]
    changes = 0

    for row in combined["rows"]:
        for backend in combined["meta"]["backends"]:
            for col in TEST_COLS:
                new_st = row.get(backend, {}).get(col)
                old_st = _baseline_status(baseline, row["client"], row["proto"], backend, col)
                if old_st == new_st:
                    continue
                if old_st is None and new_st is None:
                    continue

                changes += 1
                path = f"{row['client']}/{row['proto']}/{col} [{backend}]"

                if old_st == "pass" and new_st == "fail":
                    lines.append(f"  {C_RED}[REGRESSION]{C_RESET} {path}: was {old_st}, now {new_st}")
                elif new_st == "pass" and old_st != "pass":
                    lines.append(f"  {C_GREEN}[NEW PASS]{C_RESET}   {path}: was {old_st}, now {new_st}")
                elif old_st == "fail" and new_st != "fail":
                    lines.append(f"  {C_GREEN}[FIXED]{C_RESET}      {path}: was {old_st}, now {new_st}")
                else:
                    lines.append(f"  {C_YELLOW}[CHANGED]{C_RESET}    {path}: was {old_st}, now {new_st}")

    if changes == 0:
        lines.append(f"  {C_DIM}No changes.{C_RESET}")

    return "\n".join(lines)


# ── Main ───────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Dora DHCP Client Compatibility Matrix -- Result Formatter"
    )
    parser.add_argument("--standalone", help="Path to standalone results.json")
    parser.add_argument("--nats", help="Path to NATS results.json")
    parser.add_argument("--output-json", help="Write combined JSON to this path")
    parser.add_argument("--output-md", help="Write Markdown table to this path")
    parser.add_argument("--output-term", help="Write terminal output to this path")
    parser.add_argument("--baseline", help="Path to previous combined.json for regression comparison")
    parser.add_argument("--no-color", action="store_true", help="Disable ANSI colours")

    args = parser.parse_args()

    if args.no_color:
        global C_RESET, C_BOLD, C_GREEN, C_RED, C_YELLOW, C_CYAN, C_DIM, C_BLUE
        global SYM_PASS, SYM_FAIL, SYM_SKIP, SYM_NA, SYM_NEW_PASS, SYM_REGR
        C_RESET = C_BOLD = C_GREEN = C_RED = C_YELLOW = C_CYAN = C_DIM = C_BLUE = ""
        SYM_PASS = "  pass  "
        SYM_FAIL = "  FAIL  "
        SYM_SKIP = "  skip  "
        SYM_NA   = "   --   "
        SYM_NEW_PASS = "  +pass "
        SYM_REGR     = "  !REGR "

    standalone_data = load_json(args.standalone)
    nats_data = load_json(args.nats)

    if not standalone_data and not nats_data:
        print("Error: at least one of --standalone or --nats must be provided", file=sys.stderr)
        sys.exit(1)

    combined = build_combined(standalone_data, nats_data)

    baseline = None
    if args.baseline:
        baseline = load_json(args.baseline)

    # Terminal output (always printed)
    term_output = render_terminal(combined, baseline)
    print(term_output)

    if baseline:
        diff_output = render_diff(combined, baseline)
        print(diff_output)

    # Write outputs
    if args.output_json:
        os.makedirs(os.path.dirname(args.output_json) or ".", exist_ok=True)
        with open(args.output_json, "w") as f:
            json.dump(combined, f, indent=2)
        print(f"[format] Combined JSON written to {args.output_json}")

    if args.output_md:
        os.makedirs(os.path.dirname(args.output_md) or ".", exist_ok=True)
        md = render_markdown(combined, baseline)
        with open(args.output_md, "w") as f:
            f.write(md)
        print(f"[format] Markdown written to {args.output_md}")

    if args.output_term:
        os.makedirs(os.path.dirname(args.output_term) or ".", exist_ok=True)
        # Write without ANSI for the file
        saved = (C_RESET, C_BOLD, C_GREEN, C_RED, C_YELLOW, C_CYAN, C_DIM, C_BLUE)
        with open(args.output_term, "w") as f:
            f.write(render_terminal(combined, baseline).replace(C_RESET, "").replace(C_BOLD, "").replace(C_GREEN, "").replace(C_RED, "").replace(C_YELLOW, "").replace(C_CYAN, "").replace(C_DIM, "").replace(C_BLUE, ""))
        print(f"[format] Terminal output written to {args.output_term}")

    # Exit with error if any backend has failures
    has_failures = False
    for _, s in combined.get("summary", {}).items():
        if s.get("failed", 0) > 0:
            has_failures = True
    if has_failures:
        sys.exit(1)


if __name__ == "__main__":
    main()
