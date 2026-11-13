"""The image's numpy and pandas, through a round trip each."""

import numpy as np
import pandas as pd

a = np.arange(1000.0).reshape(100, 10)
print(float((a @ a.T).sum()))
df = pd.DataFrame({"a": range(100), "b": [x * 0.5 for x in range(100)]})
df.to_csv("pd.csv", index=False)
assert pd.read_csv("pd.csv").shape == (100, 2)
df.to_excel("pd.xlsx", index=False)
assert pd.read_excel("pd.xlsx").shape == (100, 2)
