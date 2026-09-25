-- fires
-- Movie 1 has keyword 1 twice. MIN does not care how often a row joins, which is why the reduction is allowed at all.
SELECT MIN(t.id) AS movie, MAX(k.id) AS keyword FROM title AS t, movie_keyword AS mk, keyword AS k
WHERE t.id = mk.movie_id AND mk.keyword_id = k.id AND k.keyword = 'sequel';
