import unittest

from widget import label, title


class WidgetTests(unittest.TestCase):
    def test_label(self):
        self.assertEqual(label(), "name")

    def test_title(self):
        self.assertEqual(title(), "title")


if __name__ == "__main__":
    unittest.main()
