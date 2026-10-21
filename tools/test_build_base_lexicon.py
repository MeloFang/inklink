# -*- coding: utf-8 -*-
"""parse_dict_text 的红线测试。

期望值全部是手写的字面量（独立真相源），不由被测代码推导。
运行：在 tools/ 目录下执行 `python -m unittest -v`
"""
import unittest

from build_base_lexicon import parse_dict_text


class ParseDictTextTest(unittest.TestCase):
    SAMPLE = (
        "# Rime dictionary\n"
        "name: demo\n"
        "version: '1'\n"
        "...\n"
        "\n"
        "# 表体里的注释也要跳过\n"
        "的\tde\t76938354\n"
        "项目\txiang mu\t35\n"
        "坏行没有制表符\n"
        "只有词\t\n"
        "权重坏了\tabc\txx\n"
    )

    def test_只保留完整且权重合法的行(self):
        self.assertEqual(
            parse_dict_text(self.SAMPLE),
            [("的", "de", 76938354), ("项目", "xiang mu", 35)],
        )

    def test_缺少表体标记时报错(self):
        with self.assertRaises(ValueError):
            parse_dict_text("name: demo\nversion: '1'\n")

    def test_空表体返回空列表(self):
        self.assertEqual(parse_dict_text("...\n"), [])


if __name__ == "__main__":
    unittest.main()
