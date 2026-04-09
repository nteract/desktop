import type { CellData } from "../types";

/** Single cell: execute_cell with a plotly chart */
export const singleCellPlotly: CellData = {
	cell_id: "cell-a1b2c3d4",
	cell_type: "code",
	source: "import plotly.express as px\nfig = px.scatter(df, x='date', y='gap_minutes')\nfig.show()",
	execution_count: 3,
	status: "done",
	outputs: [
		{
			output_type: "display_data",
			data: {
				"application/vnd.plotly.v1+json": '{"data":[{"x":[1,2,3],"y":[4,5,6],"type":"scatter"}],"layout":{"title":"Gap Analysis"}}',
				"text/plain": "Figure()",
			},
		},
	],
};

/** Single cell: error output */
export const singleCellError: CellData = {
	cell_id: "cell-e5f6g7h8",
	cell_type: "code",
	source: "import pandas as pd\nimport numpy as np",
	execution_count: 1,
	status: "error",
	outputs: [
		{
			output_type: "error",
			ename: "ModuleNotFoundError",
			evalue: "No module named 'pandas'",
			traceback: [
				"Traceback (most recent call last):",
				'  File "<stdin>", line 1, in <module>',
				"ModuleNotFoundError: No module named 'pandas'",
			],
		},
	],
};

/** Multi-cell: run_all_cells with mixed outputs */
export const multiCellRun: CellData[] = [
	{
		cell_id: "cell-m1",
		cell_type: "code",
		source: "df = pd.read_csv('data.csv')\nprint(f'{len(df)} rows × {len(df.columns)} cols')",
		execution_count: 1,
		status: "done",
		outputs: [
			{
				output_type: "stream",
				name: "stdout",
				text: "148,020 rows × 13 cols\nDate range: 2026-03-01 00:15 – 2026-03-31 23:58",
			},
		],
	},
	{
		cell_id: "cell-m2",
		cell_type: "code",
		source: "import pandas as pd\nimport numpy as np\nimport matplotlib.pyplot as plt",
		execution_count: 2,
		status: "error",
		outputs: [
			{
				output_type: "error",
				ename: "ModuleNotFoundError",
				evalue: "No module named 'pandas'",
				traceback: [
					"Traceback (most recent call last):",
					'  File "<stdin>", line 1, in <module>',
					"ModuleNotFoundError: No module named 'pandas'",
				],
			},
		],
	},
	{
		cell_id: "cell-m3",
		cell_type: "code",
		source: "fig = px.imshow(heatmap_data, title='Reports per Hour')\nfig.show()",
		execution_count: 3,
		status: "done",
		outputs: [
			{
				output_type: "display_data",
				data: {
					"text/html": "<div style='padding:20px;background:#2d3748;border-radius:8px;text-align:center;color:#a0aec0;'>📊 Reports per Hour — March 2026<br/><br/><div style='display:flex;gap:2px;align-items:flex-end;height:80px;justify-content:center'><div style='width:20px;background:#3b82f6;height:40%;border-radius:2px 2px 0 0;opacity:0.3'></div><div style='width:20px;background:#3b82f6;height:55%;border-radius:2px 2px 0 0;opacity:0.4'></div><div style='width:20px;background:#3b82f6;height:80%;border-radius:2px 2px 0 0;opacity:0.6'></div><div style='width:20px;background:#3b82f6;height:100%;border-radius:2px 2px 0 0;opacity:0.9'></div><div style='width:20px;background:#3b82f6;height:70%;border-radius:2px 2px 0 0;opacity:0.5'></div><div style='width:20px;background:#ef4444;height:10%;border-radius:2px 2px 0 0;opacity:0.8'></div><div style='width:20px;background:#3b82f6;height:45%;border-radius:2px 2px 0 0;opacity:0.4'></div></div></div>",
					"text/plain": "Figure()",
				},
			},
		],
	},
	{
		cell_id: "cell-m4",
		cell_type: "code",
		source: "print(f'Biggest gap: {gaps.max():.0f} min ({gaps.max()/60:.1f} hrs)')\nprint(f'{len(big_gaps)} gaps over 60 minutes')",
		execution_count: 4,
		status: "done",
		outputs: [
			{
				output_type: "stream",
				name: "stdout",
				text: "Biggest gap: 164 min (2.7 hrs)\n18 gaps over 60 minutes",
			},
		],
	},
	{
		cell_id: "cell-m5",
		cell_type: "code",
		source: "# This cell was cancelled because cell 2 errored",
		execution_count: null,
		status: "cancelled",
		outputs: [],
	},
];

/** Single cell: text-only execute_result */
export const singleCellText: CellData = {
	cell_id: "cell-t1",
	cell_type: "code",
	source: "df.describe()",
	execution_count: 5,
	status: "done",
	outputs: [
		{
			output_type: "execute_result",
			execution_count: 5,
			data: {
				"text/plain": "              count       mean        std  ...\ntimestamp  148020.0  1.71e+09   1.55e+06  ...\nstation_id 148020.0     38.5      22.3    ...",
				"text/html": "<table><tr><th></th><th>count</th><th>mean</th><th>std</th></tr><tr><td>timestamp</td><td>148020</td><td>1.71e+09</td><td>1.55e+06</td></tr></table>",
			},
		},
	],
};

/** Single cell: image output */
export const singleCellImage: CellData = {
	cell_id: "cell-img1",
	cell_type: "code",
	source: "plt.figure(figsize=(10, 6))\nplt.plot(x, y)\nplt.show()",
	execution_count: 7,
	status: "done",
	outputs: [
		{
			output_type: "display_data",
			data: {
				"image/png": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPj/HwADBwIAMCbHYQAAAABJRU5ErkJggg==",
				"text/plain": "<Figure size 1000x600>",
			},
		},
	],
};
