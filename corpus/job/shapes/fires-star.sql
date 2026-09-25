-- fires
-- The shape of every JOB query: MINs over a star of inner equi-joins around title, with filters on the leaves.
SELECT MIN(t.title) AS movie_title, MIN(k.keyword) AS keyword, MAX(t.production_year) AS year
FROM title AS t, movie_keyword AS mk, keyword AS k, movie_info AS mi
WHERE t.id = mk.movie_id AND mk.keyword_id = k.id AND t.id = mi.movie_id AND mi.info = 'USA' AND k.keyword IN ('sequel', 'murder');
