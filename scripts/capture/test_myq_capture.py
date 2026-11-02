"""Offline tests of the capture addon's redaction. Run: python3 -m unittest discover scripts/capture"""

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from myq_capture import body_shape, flow_record, redact_headers, redact_value  # noqa: E402

DUMMY_PASSWORD = "dummy-password-value"
DUMMY_JWT = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJkdW1teSJ9.ZHVtbXk"


class Redaction(unittest.TestCase):
    def test_credentials_in_json_are_removed(self):
        body = {
            "Password": DUMMY_PASSWORD,
            "access_token": DUMMY_JWT,
            "refresh_token": "dummy-refresh",
            "nested": {"Email": "person@example.com", "serial_number": "DUMMYSERIAL"},
        }
        text = json.dumps(body_shape("application/json", json.dumps(body)))
        for leaked in (DUMMY_PASSWORD, DUMMY_JWT, "dummy-refresh", "person@example.com", "DUMMYSERIAL"):
            self.assertNotIn(leaked, text)

    def test_structure_and_constants_survive(self):
        body = {"device_family": "garagedoor", "state": {"door_state": "closed", "online": True}, "count": 2, "x": None}
        shape = body_shape("application/json", json.dumps(body))["json"]
        self.assertEqual(shape["device_family"], "garagedoor")
        self.assertEqual(shape["count"], 2)
        self.assertIsNone(shape["x"])

    def test_token_shaped_value_is_removed_under_an_innocent_key(self):
        self.assertTrue(redact_value("blob", DUMMY_JWT).startswith("<redacted:token"))
        self.assertTrue(redact_value("blob", "Bearer abc").startswith("<redacted:token"))
        self.assertTrue(redact_value("blob", "A" * 64).startswith("<redacted:opaque"))

    def test_form_body_values_are_filtered_by_key(self):
        shape = body_shape(
            "application/x-www-form-urlencoded",
            f"Email=person%40example.com&Password={DUMMY_PASSWORD}&grant_type=authorization_code&code=dummycode",
        )
        pairs = dict(shape["form"])
        self.assertEqual(pairs["grant_type"], "authorization_code")
        self.assertTrue(pairs["Password"].startswith("<redacted"))
        self.assertTrue(pairs["Email"].startswith("<redacted"))
        self.assertTrue(pairs["code"].startswith("<redacted"))

    def test_html_keeps_form_names_and_drops_values(self):
        page = (
            "<html><head><title>Sign in</title></head><body>"
            '<form method="post" action="/login?x=1"><input name="Email" value="person@example.com">'
            f'<input name="__RequestVerificationToken" value="{DUMMY_PASSWORD}"></form></body></html>'
        )
        shape = body_shape("text/html", page)["html"]
        self.assertEqual(shape["title"], "Sign in")
        self.assertEqual(shape["forms"][0]["action"], "/login")
        self.assertEqual([i["name"] for i in shape["forms"][0]["inputs"]], ["Email", "__RequestVerificationToken"])
        self.assertNotIn(DUMMY_PASSWORD, json.dumps(shape))
        self.assertNotIn("person@example.com", json.dumps(shape))

    def test_headers_keep_cookie_names_only(self):
        out = redact_headers(
            [
                ("Authorization", f"Bearer {DUMMY_JWT}"),
                ("Cookie", "sid=dummy1; other=dummy2"),
                ("Set-Cookie", "sid=dummy3; Path=/"),
                ("Set-Cookie", "second=dummy4; Path=/"),
                ("User-Agent", "okhttp/4"),
            ]
        )
        self.assertEqual(out["Cookie"], ["sid", "other"])
        self.assertEqual(out["Set-Cookie"], ["sid", "second"])
        self.assertEqual(out["User-Agent"], "okhttp/4")
        self.assertNotIn(DUMMY_JWT, json.dumps(out))

    def test_whole_record_never_contains_the_secrets(self):
        record = flow_record(
            "POST",
            "example.invalid",
            "/token",
            [("code", "dummycode"), ("client_id", "app_constant")],
            [("Authorization", f"Bearer {DUMMY_JWT}")],
            "application/json",
            json.dumps({"password": DUMMY_PASSWORD}),
            200,
            [("Set-Cookie", "sid=dummy3")],
            "application/json",
            json.dumps({"access_token": DUMMY_JWT}),
        )
        text = json.dumps(record)
        for leaked in (DUMMY_PASSWORD, DUMMY_JWT, "dummycode", "dummy3"):
            self.assertNotIn(leaked, text)
        self.assertIn("app_constant", text)


if __name__ == "__main__":
    unittest.main()
