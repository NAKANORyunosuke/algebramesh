"""The job records in the state of one scheduler, as one digest (Issue #46).

    python3 records_digest.py <metadata.sqlite3>

Prints the number of jobs and the SHA-256 of every job id and `record_json`,
in the order of the job ids. Two schedulers that have applied the same changes
print the same line; a scheduler that dropped or added a field of a record
prints another. The database is opened read-only, also while the scheduler
runs.
"""

import hashlib
import sqlite3
import sys


def digest(path):
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        rows = db.execute("SELECT job_id, record_json FROM jobs ORDER BY job_id").fetchall()
    finally:
        db.close()
    h = hashlib.sha256()
    for job_id, record in rows:
        h.update(job_id.encode())
        h.update(b"\0")
        h.update(record.encode())
        h.update(b"\n")
    return len(rows), h.hexdigest()


if __name__ == "__main__":
    n, d = digest(sys.argv[1])
    print(f"{n} {d}")
