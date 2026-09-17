"""Real HTTP exchanges: credentials never accompany routine job requests."""

import json
import threading
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError

import pytest
from workshop_agent.agent import Api


@contextmanager
def server(replies):
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            requests.append((self.path, self.headers, json.loads(body) if body else None))
            code, payload, headers = replies.pop(0)
            self.send_response(code)
            for key, value in headers.items():
                self.send_header(key, value)
            self.end_headers()
            self.wfile.write(json.dumps(payload).encode())

        def log_message(self, *_args):
            pass

    httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=httpd.serve_forever)
    thread.start()
    try:
        yield f"http://127.0.0.1:{httpd.server_port}", requests
    finally:
        httpd.shutdown()
        httpd.server_close()
        thread.join()


def session(token):
    return (200, {"access_token": token, "expires_in": 900}, {})


def test_session_reuse_renewal_and_bounded_result(monkeypatch):
    clock = [1000]
    monkeypatch.setattr("workshop_agent.agent.time.monotonic", lambda: clock[0])
    with server(
        [session("one"), (204, None, {}), (200, {}, {}), session("two"), (204, None, {})]
    ) as (url, requests):
        api = Api(url, "secret", "DEMO0000001")
        assert api.take() is None
        api.report(7, 4, result="x" * 5000)
        clock[0] += 901
        assert api.take() is None
    assert [headers["Authorization"] for _, headers, _ in requests] == [
        "Agent secret",
        "Bearer one",
        "Bearer one",
        "Agent secret",
        "Bearer two",
    ]
    assert [path for path, _, _ in requests] == [
        "/integrations/fiscal/agent/session/",
        "/integrations/fiscal/agent/jobs/take/",
        "/integrations/fiscal/agent/jobs/7/result/",
        "/integrations/fiscal/agent/session/",
        "/integrations/fiscal/agent/jobs/take/",
    ]
    assert all(headers["X-Device-Serial"] == "DEMO0000001" for _, headers, _ in requests)
    assert len(requests[2][2]["result"]) == 2000


def test_unauthorized_refreshes_once_and_never_loops():
    with server([session("one"), (401, {}, {}), session("two"), (401, {}, {})]) as (url, requests):
        with pytest.raises(HTTPError) as exc:
            Api(url, "secret", "DEMO0000001").take()
        assert exc.value.code == 401
    assert [headers["Authorization"] for _, headers, _ in requests] == [
        "Agent secret",
        "Bearer one",
        "Agent secret",
        "Bearer two",
    ]


@pytest.mark.parametrize("code", [302, 500])
def test_redirects_and_server_errors_do_not_replay_a_job(code):
    with server([session("one"), (code, {}, {"Location": "/steal-credential"})]) as (url, requests):
        with pytest.raises(HTTPError) as exc:
            Api(url, "secret", "DEMO0000001").take()
        assert exc.value.code == code
    assert len(requests) == 2


@pytest.mark.parametrize(
    "url",
    [
        "ftp://server",
        "http://server",
        "https://user:pass@server",
        "https://server?token=secret",
        "https://server/#fragment",
        "https:///missing-host",
    ],
)
def test_unsafe_api_urls_are_rejected(url):
    with pytest.raises(ValueError):
        Api(url, "secret", "DEMO0000001")
