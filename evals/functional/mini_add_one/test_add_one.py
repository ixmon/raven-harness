from add_one import add_one

def test_add_one():
    assert add_one(0) == 1
    assert add_one(1) == 2
    assert add_one(-1) == 0
