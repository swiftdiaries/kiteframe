from __future__ import annotations

import argparse
import os
import re
import subprocess
from pathlib import Path
from urllib.parse import parse_qsl, unquote, urlsplit, urlunsplit


DATABASE_NAME = re.compile(r"[A-Za-z0-9_]+")

LIBPQ_QUERY_ENVIRONMENT = {
    "application_name": "PGAPPNAME",
    "connect_timeout": "PGCONNECT_TIMEOUT",
    "options": "PGOPTIONS",
    "sslcert": "PGSSLCERT",
    "sslkey": "PGSSLKEY",
    "sslmode": "PGSSLMODE",
    "sslrootcert": "PGSSLROOTCERT",
}


class DatabaseCommandError(RuntimeError):
    pass


class DatabaseConnection:
    def __init__(self, maintenance_database: str, environment: dict[str, str]) -> None:
        self.maintenance_database = maintenance_database
        self.environment = environment


def isolated_database_url(database_url: str, database_name: str) -> str:
    if not DATABASE_NAME.fullmatch(database_name):
        raise ValueError("database name must contain only letters, digits, and underscores")
    if "\r" in database_url or "\n" in database_url:
        raise ValueError("database URL must not contain line breaks")

    parsed = urlsplit(database_url)
    if parsed.scheme not in {"postgres", "postgresql"} or not parsed.netloc:
        raise ValueError("database URL must be a PostgreSQL connection URL")

    return urlunsplit(parsed._replace(path=f"/{database_name}"))


def database_connection(database_url: str) -> DatabaseConnection:
    if "\r" in database_url or "\n" in database_url:
        raise ValueError("database URL must not contain line breaks")

    parsed = urlsplit(database_url)
    if parsed.scheme not in {"postgres", "postgresql"} or not parsed.netloc:
        raise ValueError("database URL must be a PostgreSQL connection URL")
    if parsed.hostname is None:
        raise ValueError("database URL must include a host")
    maintenance_database = unquote(parsed.path.removeprefix("/"))
    if not maintenance_database:
        raise ValueError("database URL must include a database name")

    try:
        port = parsed.port
    except ValueError as error:
        raise ValueError("database URL must include a valid port") from error

    environment = os.environ.copy()
    environment.update(
        {
            "PGDATABASE": maintenance_database,
            "PGHOST": parsed.hostname,
            "PGPORT": str(port or 5432),
        }
    )
    if parsed.username is not None:
        environment["PGUSER"] = unquote(parsed.username)
    if parsed.password is not None:
        environment["PGPASSWORD"] = unquote(parsed.password)
    else:
        environment.pop("PGPASSWORD", None)
    for option, value in parse_qsl(parsed.query, keep_blank_values=True):
        if environment_name := LIBPQ_QUERY_ENVIRONMENT.get(option):
            environment[environment_name] = value

    return DatabaseConnection(maintenance_database, environment)


def run_database_command(arguments: list[str], environment: dict[str, str]) -> None:
    try:
        subprocess.run(
            arguments,
            check=True,
            env=environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except (OSError, subprocess.CalledProcessError):
        raise DatabaseCommandError("PostgreSQL database command failed") from None


def create_database(database_url: str, database_name: str, github_output: Path) -> None:
    smoke_url = isolated_database_url(database_url, database_name)
    connection = database_connection(database_url)
    run_database_command(
        ["createdb", "--maintenance-db", connection.maintenance_database, database_name],
        connection.environment,
    )
    print(f"::add-mask::{smoke_url}")
    with github_output.open("a", encoding="utf-8") as output:
        output.write(f"database_url={smoke_url}\n")


def drop_database(database_url: str, database_name: str) -> None:
    isolated_database_url(database_url, database_name)
    connection = database_connection(database_url)
    run_database_command(
        [
            "dropdb",
            "--maintenance-db",
            connection.maintenance_database,
            "--if-exists",
            "--force",
            database_name,
        ],
        connection.environment,
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

    try:
        if args.action == "create":
            if args.github_output is None:
                parser.error("--github-output is required when creating a database")
            create_database(args.database_url, args.database_name, args.github_output)
        else:
            drop_database(args.database_url, args.database_name)
    except (DatabaseCommandError, ValueError):
        parser.exit(1, "isolated database command failed\n")


if __name__ == "__main__":
    main()
