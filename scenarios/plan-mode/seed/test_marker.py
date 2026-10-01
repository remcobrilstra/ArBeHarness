import unittest

from marker import marker


class MarkerTests(unittest.TestCase):
    def test_marker(self):
        self.assertEqual(marker(), "final")


if __name__ == "__main__":
    unittest.main()
