print("hey")

print("what is so slow now???")


class WellNow:
    def __init__(self):
        self.message = "Well now!"

    def __repr__(self):
        return self.message


wn = WellNow()


# Pandas DataFrame - should render as a markdown table
import pandas as pd

df = pd.DataFrame(
    {
        "Name": ["Alice", "Bob", "Charlie", "Dave", "Quill"],
        "Age": [25, 30, 35, 40, 45],
        "City": ["NYC", "LA", "Chicago", "Houston", "Seattle"],
    }
)
df
