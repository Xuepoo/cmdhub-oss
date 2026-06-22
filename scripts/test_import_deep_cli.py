import importlib.util
import pathlib
import sys

spec = importlib.util.spec_from_file_location(
    "idc", str(pathlib.Path(__file__).parent / "import_deep_cli.py"))
idc = importlib.util.module_from_spec(spec)
sys.modules["idc"] = idc
spec.loader.exec_module(idc)


def _desc(text):
    return idc._extract_description(idc._clean(text))


def test_skips_synopsis_picks_prose():
    # GNU install: usage + "or:" synopsis lines, then the real prose summary.
    t = (
        "Usage: install [OPTION]... [-T] SOURCE DEST\n"
        "  or:  install [OPTION]... SOURCE... DIRECTORY\n"
        "  or:  install [OPTION]... -d DIRECTORY...\n\n"
        "This install program copies files into destination locations.\n"
    )
    assert _desc(t) == "This install program copies files into destination locations."


def test_skips_flag_dump_picks_prose():
    # A flag line must not become the description; the prose above it wins.
    t = (
        "Lint shell scripts for common mistakes.\n\n"
        "  -a --check-sourced  Include warnings from sourced files\n"
        "  -e CODE --exclude=CODE  Exclude types of warnings\n"
    )
    assert _desc(t) == "Lint shell scripts for common mistakes."


def test_flag_only_help_yields_no_false_description():
    # Pure flag/usage dump with no prose (pyfiglet-style) -> empty, not a flag line.
    t = (
        "Usage: pyfiglet [options] text..\n\n"
        "  --version  show program's version number and exit\n"
        "  -f FONT, --font=FONT  font to render with\n"
    )
    assert _desc(t) == ""


def test_skips_version_banner():
    t = "fonttools v4.63.0\n\nManipulate font files from Python.\n"
    assert _desc(t) == "Manipulate font files from Python."


def test_skips_copyright_banner():
    t = (
        "Copyright (c) 1990-2008 Info-ZIP - Type 'zip -L' for license.\n\n"
        "Zip puts one or more compressed files into a single zip archive.\n"
    )
    assert _desc(t) == "Zip puts one or more compressed files into a single zip archive."


def test_skips_bare_section_label():
    # az-style: a lone "Group" label is not a description.
    t = "Group\n    az\n\nSubgroups:\n    account : Manage subscriptions.\n"
    # No prose summary exists for the az root -> empty (caller falls back), never "Group".
    assert _desc(t) != "Group"


def test_keeps_normal_prose():
    # A clean description starting with a normal word is unaffected.
    t = "Usage: tar [OPTION...] [FILE]...\n\nGNU tar saves many files into one archive.\n"
    assert _desc(t) == "GNU tar saves many files into one archive."
