"""Fixture module for the pyright half of the raw LSP spike."""

# @decl DEFAULT_LIMIT | DEFAULT_LIMIT
DEFAULT_LIMIT = 3


# @decl Config | Config
class Config:
    # @decl __init__ | __init__
    def __init__(self, limit: int = DEFAULT_LIMIT) -> None:
        # @decl limit | limit
        self.limit = limit

    # @decl doubled | doubled
    def doubled(self) -> int:
        # @expr limit | self.limit * 2
        return self.limit * 2


# @decl Label | Label
class Label:
    # @decl label | label
    def label(self) -> str:
        return "label"


# @decl choose | choose
def choose(flag: bool) -> int:
    # @expr flag | 1 if flag else 2
    return 1 if flag else 2


# @decl identity | identity
def identity(value: int) -> int:
    # @expr value | value
    return value
