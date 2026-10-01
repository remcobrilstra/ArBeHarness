import unittest

from pricing import price_for


class PricingTests(unittest.TestCase):
    def test_member_mug(self):
        self.assertEqual(price_for("mug", member=True), 850)


if __name__ == "__main__":
    unittest.main()
