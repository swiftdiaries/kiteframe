from __future__ import annotations

import argparse
import re
import subprocess
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit


DATABASE_NAME = re.compile(r"[A-Za-z0-9_]+")


def isolated_database_url(database_url: str, database_name: str) -> str:
    if not DATABASE_NAME.fullmatch(database_name):
        raise ValueError("database name must contain only letters, digits, and underscores")
    if "\r" in database_url or "\n" in database_url:
        raise ValueError("database URL must not contain line breaks")

    parsed = urlsplit(database_url)
    if parsed.scheme not in {"postgres", "postgresql"} or not parsed.netloc:
        raise ValueError("database URL must be a PostgreSQL connection URL")

    return urlunsplit(parsed._replace(path=f"/{database_name}"))


def create_database(database_url: str, database_name: str, github_output: Path) -> None:
    smoke_url = isolated_database_url(database_url, database_name)
    subprocess.run(
        ["createdb", f"--maintenance-db={database_url}", database_name],
        check=True,
    )
    with github_output.open("a", encoding="utf-8") as output:
        output.write(f"database_url={smoke_url}\n")


def drop_database(database_url: str, database_name: str) -> None:
    isolated_database_url(database_url, database_name)
    subprocess.run(
        [
            "dropdb",
            f"--maintenance-db={database_url}",
            "--if-exists",
            "--force",
            database_name,
        ],
        check=True,
    )


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Create or drop an isolated PostgreSQL database for a CI smoke test."
    )
    parser.add_argument("action", choices=("create", "drop"))
    parser.add_argument("--database-url", required=True)
    parser.add_argument("--database-name", required=True)
    parser.add_argument("--github-output", type=Path)
    args = parser.parse_args()

    if args.action == "create":
        if args.github_output is None:
            parser.error("--github-output is required when creating a database")
        create_database(args.database_url, args.database_name, args.github_output)
    else:
        drop_database(args.database_url, args.database_name)


if __name__ == "__main__":
    main()
