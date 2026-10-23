"""Run isolated real-component checks against this project's environment."""
import concurrent.futures
import copy
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
EVIDENCE = ROOT / "testwork" / "evidence"
EVIDENCE.mkdir(parents=True, exist_ok=True)
PROFILE = "release" if "--release" in sys.argv else "debug"
EXE = ROOT / f"target/x86_64-pc-windows-msvc/{PROFILE}/flaragon-cli.exe"
results = []


def user_config(text):
    """Extract the editable native directives without instance-managed sections."""
    lines = text.splitlines(keepends=True)
    start = next(i for i, line in enumerate(lines) if 'END FLARAGON INSTANCE' in line) + 1
    end = next((i for i in range(start, len(lines)) if 'BEGIN FLARAGON PROJECTS' in lines[i]), len(lines))
    return ''.join(lines[start:end])


class Session:
    def __init__(self):
        self.proc = subprocess.Popen([str(EXE), str(ROOT)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=(EVIDENCE / "cli-stderr.log").open("w", encoding="utf-8"),
                                     text=True, encoding="utf-8", cwd=ROOT, creationflags=subprocess.CREATE_NO_WINDOW)
        self.snapshot = None

    def call(self, action, expect=True, **kwargs):
        self.proc.stdin.write(json.dumps(dict(action=action, **kwargs), ensure_ascii=False) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("CLI exited unexpectedly")
        response = json.loads(line)
        self.snapshot = response.get("snapshot")
        if response["ok"] != expect:
            raise RuntimeError(f"{action}: {response.get('error', 'unexpected success')}")
        if not response["ok"]:
            print("Expected rejection:", response["error"], flush=True)
        return response

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=15)


def record(name, detail=""):
    results.append(dict(name=name, passed=True, detail=detail))
    print("PASS", name, detail, flush=True)


def http(domain, path="/", method="GET", body=None, headers=None):
    connection = HTTPConnection("127.0.0.1", 18080, timeout=12)
    request_headers = {"Host": domain}
    request_headers.update(headers or {})
    connection.request(method, path, body=body, headers=request_headers)
    response = connection.getresponse()
    data = response.read().decode("utf-8")
    connection.close()
    if response.status != 200:
        raise AssertionError(f"HTTP {response.status}: {data[:500]}")
    return data


def redis(*args):
    wire = f"*{len(args)}\r\n" + "".join(f"${len(a.encode())}\r\n{a}\r\n" for a in args)
    with socket.create_connection(("127.0.0.1", 16379), timeout=5) as connection:
        connection.sendall(wire.encode())
        return connection.recv(8192).decode()


def mysql(package, port, sql):
    executable = Path(package["directory"]) / "bin/mysql.exe"
    env = {"SystemRoot": os.environ.get("SystemRoot", "C:\\Windows"), "TEMP": str(ROOT / "runtime/temp"),
           "TMP": str(ROOT / "runtime/temp"), "USERPROFILE": str(ROOT / "runtime"), "APPDATA": str(ROOT / "runtime")}
    process = subprocess.run([str(executable), "--no-defaults", "--protocol=TCP", "--host=127.0.0.1",
                              f"--port={port}", "--user=root", "--batch", "--skip-column-names", "--default-character-set=utf8mb4"],
                             input=sql, text=True, encoding="utf-8", capture_output=True, timeout=15,
                             env=env, cwd=ROOT, creationflags=subprocess.CREATE_NO_WINDOW)
    if process.returncode:
        raise RuntimeError(process.stderr)
    return process.stdout.strip()


def main():
    session = Session()
    try:
        session.call("scan")
        snapshot = session.snapshot
        assert len(snapshot["packages"]) == 7 and all(p["error"] is None for p in snapshot["packages"])
        config = copy.deepcopy(snapshot["config"])
        config["http_port"] = 18080
        config["projects"] = []
        config["mysql"] = [dict(id="accept57", package="mysql-mysql-5-7-44", port=13307, usage="common", name="accept57", series="5.7", reuse_data=True),
                           dict(id="accept80", package="mysql-mysql-8-0-44", port=13308, usage="common", name="accept80", series="8.0", reuse_data=True),
                           dict(id="accept57b", package="mysql-mysql-5-7-44", port=13309, usage="common", name="accept57b", series="5.7", reuse_data=True)]
        config["redis"].update(port=16379, usage="common")
        for index, php in enumerate(config["php"]):
            php["port"] = 19100 + index * 8
            directory = ROOT / "www" / f"验收 项目 {index}"
            public = directory / "public"
            public.mkdir(parents=True, exist_ok=True)
            (directory / "alternate").mkdir(exist_ok=True)
            fixture = """<?php
header('Content-Type: application/json');
echo json_encode(['version'=>PHP_VERSION, 'memory'=>ini_get('memory_limit'), 'root'=>$_SERVER['DOCUMENT_ROOT'], 'path'=>$_SERVER['PATH_INFO']??'', 'query'=>$_GET, 'upload'=>isset($_FILES['file'])?file_get_contents($_FILES['file']['tmp_name']):null]);
"""
            (public / "index.php").write_text(fixture, encoding="utf-8")
            (directory / "alternate/index.php").write_text(fixture, encoding="utf-8")
            (public / "static.txt").write_text("flaragon-static-ok", encoding="utf-8")
            (public / ".htaccess").write_text("RewriteEngine On\nRewriteRule ^hello$ index.php?rewritten=yes [L,QSA]\n", encoding="utf-8")
            (public / "redis.php").write_text("<?php $s=fsockopen('127.0.0.1',16379);fwrite($s,\"*2\\r\\n$3\\r\\nGET\\r\\n$13\\r\\nflaragon-test\\r\\n\");echo fgets($s).fgets($s);fclose($s);", encoding="utf-8")
            config["projects"].append(dict(id=f"accept{index}", domain=f"accept{index}.localhost", virtual_host=True, directory=str(directory.relative_to(ROOT)), document_root="public", php=php["package"], enabled=True))
        session.call("apply", config=config)
        record("All seven component packages discovered", ", ".join(p["version"] for p in snapshot["packages"]))
        session.call("start_all")
        record("Concurrent Apache, three PHP pools, three MySQL instances, Redis")
        session.call("verify")
        packages = {p["id"]: p for p in snapshot["packages"]}
        for index, php in enumerate(config["php"]):
            domain = f"accept{index}.localhost"
            info = json.loads(http(domain))
            assert info["version"] == packages[php["package"]]["version"]
            assert http(domain, "/static.txt") == "flaragon-static-ok"
            assert json.loads(http(domain, "/hello"))["query"]["rewritten"] == "yes"
            assert json.loads(http(domain, "/index.php/path/info"))["path"] == "/path/info"
            body = '--FlaragonBoundary\r\nContent-Disposition: form-data; name="file"; filename="sample.txt"\r\nContent-Type: text/plain\r\n\r\nupload-ok\r\n--FlaragonBoundary--\r\n'
            assert json.loads(http(domain, "/", "POST", body, {"Content-Type": "multipart/form-data; boundary=FlaragonBoundary"}))["upload"] == "upload-ok"
            record(f"PHP {info['version']}: Chinese/spaced public root, static, Rewrite, PATH_INFO, upload")
        with concurrent.futures.ThreadPoolExecutor(max_workers=12) as pool:
            versions = list(pool.map(lambda i: json.loads(http(f"accept{i%3}.localhost"))["version"], range(60)))
        assert len(versions) == 60
        record("60 concurrent HTTP requests across three PHP versions")
        for index, spec in enumerate(config["mysql"]):
            package = packages[spec["package"]]
            actual = mysql(package, spec["port"], f"CREATE DATABASE IF NOT EXISTS flaragon_acceptance; USE flaragon_acceptance; CREATE TABLE IF NOT EXISTS marker(id INT PRIMARY KEY, value INT); REPLACE INTO marker VALUES(1,{index}); SELECT value FROM marker WHERE id=1;")
            assert actual == str(index)
        record("MySQL 5.7/8.0 and two same-version instances: isolated writes")
        assert redis("SET", "flaragon-test", "persist-ok").startswith("+OK")
        for index in range(3):
            assert "persist-ok" in http(f"accept{index}.localhost", "/redis.php")
        record("Redis shared across PHP 7.4/8.3/8.5 via RESP")
        session.call("stop_all")
        assert all(row["state"] == "已停止" for row in session.snapshot["rows"])
        session.call("start_all")
        assert "persist-ok" in redis("GET", "flaragon-test")
        for index, spec in enumerate(config["mysql"]):
            assert mysql(packages[spec["package"]], spec["port"], "SELECT value FROM flaragon_acceptance.marker WHERE id=1;") == str(index)
        record("Normal stop/restart with MySQL and Redis persistence")
        if "--smoke" in sys.argv:
            return
        config = copy.deepcopy(session.snapshot["config"])
        config["projects"][0]["document_root"] = "alternate"
        session.call("apply", config=config)
        assert json.loads(http("accept0.localhost"))["root"].endswith("/alternate")
        record("Document root applies to actual HTTP requests")
        php = config["php"][0]["package"]
        session.call("override", id=f"php:{php}", text="memory_limit=192M\ndate.timezone=Asia/Shanghai\n")
        assert json.loads(http("accept0.localhost"))["memory"] == "192M"
        record("PHP ini applies to running workers")
        session.call("override", expect=False, id="apache", text="Timeout invalid-number\n")
        session.call("verify")
        record("Invalid Apache configuration preserves working services")
        with socket.socket() as occupied:
            occupied.bind(("127.0.0.1", 18081))
            occupied.listen()
            candidate = copy.deepcopy(session.snapshot["config"])
            candidate["http_port"] = 18081
            session.call("apply", expect=False, config=candidate)
            session.call("verify")
        record("Running Apache settings cannot be changed; live configuration remains intact")
        before = {row["id"]: row["detail"] for row in session.snapshot["rows"]}
        session.call("start_all")
        assert before == {row["id"]: row["detail"] for row in session.snapshot["rows"]}
        record("Repeated start does not create duplicate processes")
    finally:
        try:
            session.call("stop_all")
            record("All owned services stopped cleanly")
        finally:
            session.close()
            (EVIDENCE / "acceptance.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
